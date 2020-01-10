// Copyright (C) 2019 Alibaba Cloud. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD file.

//! A simple vector based epoll event manager which only supports static slot allocation.
//!
//! For VMMs which don't support device hot-addition/removal, a simple epoll event manager which
//! only supports static allocation should be good enough. On startup a group of epoll slots are
//! allocated for each consumer/device, and those allocated slots won't be freed.
//! For simplicity and performance, vector is used to store registered handlers and there's no
//! synchronization mechanism adopted, so the statically allocated slot group could be
//! accessed/used by a single thread.

use std::os::unix::io::RawFd;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::{
    EpollEventMgr, EpollHandler, EpollHandlerPayload, EpollSlot, EpollSlotAllocator,
    EpollSlotGroup, Error, Result, MAX_EPOLL_SLOTS,
};

struct MaybeHandler {
    handler: Option<Box<EpollHandler>>,
    receiver: Receiver<Box<EpollHandler>>,
}

impl MaybeHandler {
    fn new(handler: Option<Box<EpollHandler>>, receiver: Receiver<Box<EpollHandler>>) -> Self {
        MaybeHandler { handler, receiver }
    }
}

/// A simple vector based event manager which only supports static slot allocation.
pub struct EpollEventMgrVector {
    epoll_raw_fd: RawFd,
    event_handlers: Vec<MaybeHandler>,
    dispatch_table: Vec<(usize, EpollSlot)>,
}

impl EpollEventMgrVector {
    /// Create a new EpollEventMgrVector object.
    ///
    /// If `cloexec` is true, FD_CLOEXEC will be set on the underline epoll fd.
    pub fn new(cloexec: bool, capacity: usize) -> Self {
        // Just assume always success to create epoll fd.
        let epoll_raw_fd = epoll::create(cloexec).expect("failed to create epoll fd");

        EpollEventMgrVector {
            epoll_raw_fd,
            event_handlers: Vec::with_capacity(capacity),
            dispatch_table: Vec::with_capacity(capacity),
        }
    }

    fn get_handler(&mut self, event_idx: usize) -> Result<&mut EpollHandler> {
        let maybe = &mut self.event_handlers[event_idx];
        match maybe.handler {
            Some(ref mut v) => Ok(v.as_mut()),
            None => {
                // This should only be called in response to an epoll trigger.
                // Moreover, this branch of the match should only be active on the first call
                // (the first epoll event for this device), therefore the channel is guaranteed
                // to contain a message for the first epoll event since both epoll event
                // registration and channel send() happen in the device activate() function.
                let received = maybe
                    .receiver
                    .try_recv()
                    .map_err(|_| Error::HandlerNotReady)?;
                Ok(maybe.handler.get_or_insert(received).as_mut())
            }
        }
    }

    fn poll_events(&mut self, events: &mut [epoll::Event], timeout: i32) -> Result<(usize, usize)> {
        let mut handled = 0;
        let mut unknown = 0;

        let num_events =
            epoll::wait(self.epoll_raw_fd, timeout, events).map_err(Error::EpollWait)?;
        for event in events.iter().take(num_events) {
            let dispatch_idx = event.data as usize;
            if dispatch_idx >= self.dispatch_table.len() {
                unknown += 1;
            } else {
                let (handler_idx, slot) = self.dispatch_table[dispatch_idx];
                // It's serious issue if fails to get handler, shouldn't happen.
                let handler = self.get_handler(handler_idx)?;
                // Handler shouldn't return error, there's no common way for the manager to recover from failure.
                handler
                    .handle_event(slot, event.events, EpollHandlerPayload::Empty)
                    .expect("epoll event handler should not return error.");
                handled += 1;
            }
        }

        Ok((handled, unknown))
    }
}

impl EpollEventMgr for EpollEventMgrVector {
    type A = EpollSlotAllocatorVector;

    fn get_allocator(
        &mut self,
        num_slots: Option<usize>,
        handler: Option<Box<EpollHandler>>,
    ) -> Result<Self::A> {
        let count = match num_slots {
            Some(val) => {
                if val > MAX_EPOLL_SLOTS {
                    return Err(Error::TooManySlots(val));
                } else if val == 0 {
                    return Err(Error::InvalidParameter);
                }
                val
            }
            None => {
                return Err(Error::InvalidParameter);
            }
        };

        let dispatch_base = self.dispatch_table.len() as u64;
        let handler_idx = self.event_handlers.len();
        let (sender, receiver) = channel();

        for x in 0..count {
            self.dispatch_table.push((handler_idx, x as EpollSlot));
        }
        self.event_handlers
            .push(MaybeHandler::new(handler, receiver));

        Ok(EpollSlotAllocatorVector::new(
            dispatch_base,
            count,
            self.epoll_raw_fd,
            sender,
        ))
    }

    fn handle_events(&mut self, max_events: usize, timeout: i32) -> Result<(usize, usize)> {
        if max_events == 0 {
            return Err(Error::InvalidParameter);
        }
        let mut events = vec![epoll::Event::new(epoll::Events::empty(), 0); max_events];
        self.poll_events(&mut events, timeout)
    }

    fn inject_event(
        &mut self,
        slot: EpollSlot,
        event_flags: u32,
        payload: EpollHandlerPayload,
    ) -> Result<()> {
        if slot >= self.dispatch_table.len() as EpollSlot {
            Err(Error::SlotOutOfRange(slot))
        } else {
            let (handler_idx, slot) = self.dispatch_table[slot as usize];
            // It's serious issue if fails to get handler, shouldn't happen.
            let handler = self.get_handler(handler_idx)?;
            // Handler shouldn't return error, there's no common way for the manager to recover from failure.
            handler
                .handle_event(slot, event_flags, payload)
                .expect("epoll event handler should not return error.");
            Ok(())
        }
    }
}

impl Drop for EpollEventMgrVector {
    fn drop(&mut self) {
        let rc = unsafe { libc::close(self.epoll_raw_fd) };
        if rc != 0 {
            //error!("EpollEventMgrVector can't close event fd.");
        }
    }
}

/// Struct to statically allocate and manage epoll slots.
///
/// A group of epoll slots are statically allocated and will never be freed. So it can't support
/// device hot-removal. For simplicity and performance, there's no synchronization mechanism adopted
/// and the structure can only be accessed from a single thread.
pub struct EpollSlotAllocatorVector {
    first_slot: EpollSlot,
    num_slots: usize,
    epoll_raw_fd: RawFd,
    sender: Sender<Box<EpollHandler>>,
}

impl EpollSlotAllocatorVector {
    /// Create a new EpollSlotGroup object.
    fn new(
        first_slot: EpollSlot,
        num_slots: usize,
        epoll_raw_fd: RawFd,
        sender: Sender<Box<EpollHandler>>,
    ) -> Self {
        EpollSlotAllocatorVector {
            first_slot,
            num_slots,
            epoll_raw_fd,
            sender,
        }
    }
}

impl EpollSlotAllocator for EpollSlotAllocatorVector {
    type G = EpollSlotGroupVector;

    fn allocate(&mut self, num_slots: usize, handler: Box<EpollHandler>) -> Result<Self::G> {
        if num_slots != self.num_slots {
            return Err(Error::SlotOutOfRange(num_slots as EpollSlot));
        }
        //channel should be open and working
        self.sender
            .send(handler)
            .expect("Failed to send through the channel");
        Ok(EpollSlotGroupVector {
            first_slot: self.first_slot,
            num_slots: self.num_slots,
            epoll_raw_fd: self.epoll_raw_fd,
        })
    }

    fn free(&mut self, _group: &mut Self::G) -> Result<()> {
        Err(Error::OperationNotSupported)
    }
}

/// Struct to maintain information about a group of allocated slots.
pub struct EpollSlotGroupVector {
    first_slot: EpollSlot,
    num_slots: usize,
    epoll_raw_fd: RawFd,
}

impl EpollSlotGroup for EpollSlotGroupVector {
    fn len(&self) -> usize {
        self.num_slots
    }

    fn register(&self, fd: RawFd, slot: EpollSlot, events: epoll::Events) -> Result<()> {
        if slot >= self.num_slots as u64 || slot.checked_add(self.num_slots as u64).is_none() {
            return Err(Error::SlotOutOfRange(slot));
        }
        epoll::ctl(
            self.epoll_raw_fd,
            epoll::ControlOptions::EPOLL_CTL_ADD,
            fd,
            epoll::Event::new(events, self.first_slot + slot),
        )
        .map_err(Error::EpollCtl)
    }

    fn deregister(&self, fd: RawFd, slot: EpollSlot, events: epoll::Events) -> Result<()> {
        if slot >= self.num_slots as u64 || slot.checked_add(self.num_slots as u64).is_none() {
            return Err(Error::SlotOutOfRange(slot));
        }
        epoll::ctl(
            self.epoll_raw_fd,
            epoll::ControlOptions::EPOLL_CTL_DEL,
            fd,
            epoll::Event::new(events, self.first_slot + slot),
        )
        .map_err(Error::EpollCtl)
    }
}

#[cfg(test)]
mod tests {
    extern crate vmm_sys_util;

    use super::*;

    use std::os::unix::io::AsRawFd;
    use std::thread;
    use vmm_sys_util::eventfd::EventFd;

    struct TestEvent {
        evfd: EventFd,
        count: u64,
        write: bool,
    }

    impl TestEvent {
        fn new(write: bool) -> Self {
            TestEvent {
                evfd: EventFd::new(0).unwrap(),
                count: 0,
                write,
            }
        }
    }

    impl EpollHandler for TestEvent {
        fn handle_event(
            &mut self,
            _data: EpollSlot,
            _event_flags: u32,
            _payload: EpollHandlerPayload,
        ) -> Result<()> {
            self.count += 1;
            if self.write {
                self.evfd.write(1).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn epoll_get_allocator_test() {
        let mut ep = EpollEventMgrVector::new(true, 2);

        let err = ep.get_allocator(None, None);
        match err {
            Err(Error::InvalidParameter) => {}
            _ => panic!("error expected"),
        }

        let err = ep.get_allocator(Some(0), None);
        match err {
            Err(Error::InvalidParameter) => {}
            _ => panic!("error expected"),
        }

        let err = ep.get_allocator(Some(MAX_EPOLL_SLOTS + 1), None);
        match err {
            Err(Error::TooManySlots(count)) => {
                assert_eq!(count, MAX_EPOLL_SLOTS + 1);
            }
            _ => panic!("error expected"),
        }

        let allocator = ep.get_allocator(Some(2), None).unwrap();
        assert_eq!(ep.event_handlers.len(), 1);
        assert_eq!(ep.dispatch_table.len(), 2);
        assert_eq!(allocator.num_slots, 2);

        let allocator = ep.get_allocator(Some(2), None).unwrap();
        assert_eq!(ep.event_handlers.len(), 2);
        assert_eq!(ep.dispatch_table.len(), 4);
        assert_eq!(allocator.num_slots, 2);

        let allocator = ep.get_allocator(Some(MAX_EPOLL_SLOTS), None).unwrap();
        assert_eq!(ep.event_handlers.len(), 3);
        assert_eq!(ep.dispatch_table.len(), 4 + MAX_EPOLL_SLOTS);
        assert_eq!(allocator.num_slots, MAX_EPOLL_SLOTS);
    }

    #[test]
    fn epoll_inject_event_test() {
        let mut ep = EpollEventMgrVector::new(true, 1);

        let err = ep.inject_event(0, 0, EpollHandlerPayload::Empty);
        match err {
            Err(Error::SlotOutOfRange(slot)) => {
                assert_eq!(slot, 0);
            }
            _ => panic!("error expected"),
        }

        let handler = TestEvent::new(true);
        let evfd = handler.evfd.try_clone().unwrap();
        let _ = ep.get_allocator(Some(2), Some(Box::new(handler))).unwrap();
        assert_eq!(ep.event_handlers.len(), 1);
        assert_eq!(ep.dispatch_table.len(), 2);

        assert!(ep.inject_event(1, 0, EpollHandlerPayload::Empty).is_ok());
        let val = evfd.read().unwrap();
        assert_eq!(val, 1);

        assert!(ep.inject_event(1, 0, EpollHandlerPayload::Empty).is_ok());
        assert!(ep.inject_event(2, 0, EpollHandlerPayload::Empty).is_err());
        assert!(ep.inject_event(1, 0, EpollHandlerPayload::Empty).is_ok());
        let val = evfd.read().unwrap();
        assert_eq!(val, 2);
    }

    #[test]
    fn epoll_handle_events_test() {
        let mut ep = EpollEventMgrVector::new(true, 1);

        let err = ep.handle_events(0, 1).unwrap_err();
        match err {
            Error::InvalidParameter => {}
            _ => panic!("error expected"),
        }

        assert_eq!(ep.handle_events(1, 1).unwrap(), (0, 0));
        assert_eq!(ep.handle_events(1, 0).unwrap(), (0, 0));

        let handler = TestEvent::new(false);
        let evfd = handler.evfd.try_clone().unwrap();
        let mut allocator = ep.get_allocator(Some(2), None).unwrap();
        thread::spawn(move || {
            let fd = handler.evfd.as_raw_fd();
            let group = allocator.allocate(2, Box::new(handler)).unwrap();
            assert_eq!(group.len(), 2);
            group.register(fd, 1, epoll::Events::EPOLLIN).unwrap();
            evfd.write(1).unwrap();
        });
        ep.handle_events(10, -1).unwrap();
    }
}
