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
    EpollEventMgrT, EpollEvents, EpollHandler, EpollHandlerPayload, EpollSlot, EpollSlotAllocatorT,
    EpollSlotGroupT, EpollToken, EpollUserData, Error, MAX_EPOLL_SLOTS,
};

struct MaybeHandler<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    handler: Option<Box<EpollHandler<E = E>>>,
    receiver: Receiver<Box<EpollHandler<E = E>>>,
}

impl<E> MaybeHandler<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    fn new(
        handler: Option<Box<EpollHandler<E = E>>>,
        receiver: Receiver<Box<EpollHandler<E = E>>>,
    ) -> Self {
        MaybeHandler { handler, receiver }
    }
}

/// A simple vector based event manager which only supports static slot allocation.
pub struct EpollEventMgrVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    epoll_raw_fd: RawFd,
    event_handlers: Vec<MaybeHandler<E>>,
    dispatch_table: Vec<(usize, EpollSlot)>,
}

impl<E> EpollEventMgrVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
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

    fn get_handler(&mut self, event_idx: usize) -> Result<&mut EpollHandler<E = E>, E> {
        if event_idx >= self.event_handlers.len() {
            return Err(Error::InvalidParameter.into());
        }
        let maybe = &mut self.event_handlers[event_idx];
        match maybe.handler {
            Some(ref mut v) => Ok(v.as_mut()),
            None => {
                // This should only be called in response to an epoll trigger.
                // Moreover, this branch of the match should only be active on the first call
                // (the first epoll event for this device), therefore the channel is guaranteed
                // to contain a message for the first epoll event since both epoll event
                // registration and channel send() happen in the device activate() function.
                let received = maybe.receiver.recv().map_err(|_| Error::HandlerNotReady)?;
                Ok(maybe.handler.get_or_insert(received).as_mut())
            }
        }
    }

    fn poll_events(
        &mut self,
        events: &mut [epoll::Event],
        timeout: i32,
    ) -> std::result::Result<(usize, usize), E> {
        let mut handled = 0;
        let mut unknown = 0;

        let num_events =
            epoll::wait(self.epoll_raw_fd, timeout, events).map_err(Error::EpollWait)?;
        for event in events.iter().take(num_events) {
            let token = EpollToken(event.data as u64);
            if token.get_slot() as usize >= self.dispatch_table.len() {
                unknown += 1;
            } else {
                let (handler_idx, sub_idx) = self.dispatch_table[token.get_slot() as usize];
                // It's serious issue if fails to get handler, shouldn't happen.
                let handler = self.get_handler(handler_idx)?;
                // Handler shouldn't return error, there's no common way for the manager to recover from failure.
                handler.handle_event(
                    sub_idx,
                    EpollEvents::from_bits_truncate(event.events),
                    token.get_user_data(),
                    EpollHandlerPayload::Empty,
                )?;
                handled += 1;
            }
        }

        Ok((handled, unknown))
    }
}

impl<E> EpollEventMgrT for EpollEventMgrVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    type A = EpollSlotAllocatorVector<E>;
    type E = E;

    fn get_allocator(&mut self, num_slots: Option<EpollSlot>) -> Result<Self::A, Error> {
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

        let dispatch_base = self.dispatch_table.len() as EpollSlot;
        let handler_idx = self.event_handlers.len();
        let (sender, receiver) = channel();

        for x in 0..count as usize {
            self.dispatch_table.push((handler_idx, x as EpollSlot));
        }
        self.event_handlers.push(MaybeHandler::new(None, receiver));

        Ok(EpollSlotAllocatorVector::new(
            dispatch_base,
            count,
            self.epoll_raw_fd,
            sender,
        ))
    }

    fn handle_events(
        &mut self,
        max_events: usize,
        timeout: i32,
    ) -> Result<(usize, usize), Self::E> {
        if max_events == 0 {
            return Err(Error::InvalidParameter.into());
        }
        let mut events = vec![epoll::Event::new(epoll::Events::empty(), 0); max_events];
        self.poll_events(&mut events, timeout)
    }

    fn inject_event(
        &mut self,
        slot: EpollSlot,
        events: EpollEvents,
        data: EpollUserData,
        payload: EpollHandlerPayload,
    ) -> Result<(), E> {
        if slot >= self.dispatch_table.len() as EpollSlot {
            Err(Error::SlotOutOfRange(slot).into())
        } else {
            let (handler_idx, slot) = self.dispatch_table[slot as usize];
            // It's serious issue if fails to get handler, shouldn't happen.
            let handler = self.get_handler(handler_idx)?;
            // Handler shouldn't return error, there's no common way for the manager to recover from failure.
            handler.handle_event(slot, events, data, payload)
        }
    }
}

impl<E> Drop for EpollEventMgrVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
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
pub struct EpollSlotAllocatorVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    first_slot: EpollSlot,
    num_slots: EpollSlot,
    epoll_raw_fd: RawFd,
    sender: Sender<Box<EpollHandler<E = E>>>,
}

impl<E> EpollSlotAllocatorVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    /// Create a new EpollSlotGroup object.
    fn new(
        first_slot: EpollSlot,
        num_slots: EpollSlot,
        epoll_raw_fd: RawFd,
        sender: Sender<Box<EpollHandler<E = E>>>,
    ) -> Self {
        EpollSlotAllocatorVector {
            first_slot,
            num_slots,
            epoll_raw_fd,
            sender,
        }
    }
}

impl<E> EpollSlotAllocatorT for EpollSlotAllocatorVector<E>
where
    E: 'static + From<Error> + Send + Sync,
{
    type G = EpollSlotGroupVector;
    type E = E;

    fn allocate(
        &mut self,
        num_slots: EpollSlot,
        mut handler: Box<EpollHandler<E = Self::E>>,
    ) -> Result<Self::G, Self::E> {
        if num_slots != self.num_slots {
            return Err(Error::SlotOutOfRange(num_slots).into());
        }
        let group = EpollSlotGroupVector {
            first_slot: self.first_slot,
            num_slots: self.num_slots,
            epoll_raw_fd: self.epoll_raw_fd,
        };
        handler.set_group(Some(Box::new(group.clone())))?;
        //channel should be open and working
        self.sender
            .send(handler)
            .expect("Failed to send through the channel");
        Ok(group)
    }

    fn free(&mut self, _group: Self::G) -> Result<Box<EpollHandler<E = Self::E>>, Self::E> {
        Err(Error::OperationNotSupported.into())
    }

    fn base(&self) -> Option<EpollSlot> {
        Some(self.first_slot)
    }
}

/// A group of epoll slots allocated from EpollSlotAllocatorVector.
pub struct EpollSlotGroupVector {
    first_slot: EpollSlot,
    num_slots: EpollSlot,
    epoll_raw_fd: RawFd,
}

impl Clone for EpollSlotGroupVector {
    fn clone(&self) -> Self {
        EpollSlotGroupVector {
            first_slot: self.first_slot,
            num_slots: self.num_slots,
            epoll_raw_fd: self.epoll_raw_fd,
        }
    }
}

impl EpollSlotGroupT for EpollSlotGroupVector {
    fn base(&self) -> EpollSlot {
        self.first_slot
    }

    fn len(&self) -> usize {
        self.num_slots as usize
    }

    fn register(
        &self,
        fd: RawFd,
        slot: EpollSlot,
        data: EpollUserData,
        events: epoll::Events,
    ) -> Result<(), Error> {
        if slot >= self.num_slots || slot.checked_add(self.num_slots).is_none() {
            return Err(Error::SlotOutOfRange(slot));
        }
        epoll::ctl(
            self.epoll_raw_fd,
            epoll::ControlOptions::EPOLL_CTL_ADD,
            fd,
            epoll::Event::new(events, EpollToken::new(self.first_slot + slot, data).0),
        )
        .map_err(Error::EpollCtl)
    }

    fn deregister(
        &self,
        fd: RawFd,
        slot: EpollSlot,
        data: EpollUserData,
        events: epoll::Events,
    ) -> Result<(), Error> {
        if slot >= self.num_slots || slot.checked_add(self.num_slots).is_none() {
            return Err(Error::SlotOutOfRange(slot));
        }
        epoll::ctl(
            self.epoll_raw_fd,
            epoll::ControlOptions::EPOLL_CTL_DEL,
            fd,
            epoll::Event::new(events, EpollToken::new(self.first_slot + slot, data).0),
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
        type E = Error;

        fn handle_event(
            &mut self,
            _slot: EpollSlot,
            event_flags: EpollEvents,
            data: EpollUserData,
            payload: EpollHandlerPayload,
        ) -> Result<(), Error> {
            // epoll::Events::EPOLLIN is 0x1
            assert_eq!(event_flags.bits(), 0x1);
            assert_eq!(data, 0xA5);
            match payload {
                EpollHandlerPayload::Empty => {}
                _ => panic!("invalid payload"),
            }
            self.count += 1;
            if self.write {
                self.evfd.write(1).unwrap();
            }
            Ok(())
        }

        fn set_group(&mut self, _group: Option<Box<EpollSlotGroupT>>) -> Result<(), Error> {
            Ok(())
        }
    }

    #[test]
    fn epoll_create_event_mgr() {
        let _ = EpollEventMgrVector::<Error>::new(true, 0);
        let _ = EpollEventMgrVector::<Error>::new(false, 1);
        let _ = EpollEventMgrVector::<Error>::new(true, 256);
    }

    #[test]
    fn epoll_get_allocator_test() {
        let mut ep = EpollEventMgrVector::<Error>::new(true, 2);

        let err = ep.get_allocator(None);
        match err {
            Err(Error::InvalidParameter) => {}
            _ => panic!("error expected"),
        }

        let err = ep.get_allocator(Some(0));
        match err {
            Err(Error::InvalidParameter) => {}
            _ => panic!("error expected"),
        }

        let err = ep.get_allocator(Some(MAX_EPOLL_SLOTS + 1));
        match err {
            Err(Error::TooManySlots(count)) => {
                assert_eq!(count, MAX_EPOLL_SLOTS + 1);
            }
            _ => panic!("error expected"),
        }

        let allocator = ep.get_allocator(Some(2)).unwrap();
        assert_eq!(ep.event_handlers.len(), 1);
        assert_eq!(ep.dispatch_table.len(), 2);
        assert_eq!(allocator.num_slots, 2);

        let allocator = ep.get_allocator(Some(2)).unwrap();
        assert_eq!(ep.event_handlers.len(), 2);
        assert_eq!(ep.dispatch_table.len(), 4);
        assert_eq!(allocator.num_slots, 2);

        let allocator = ep.get_allocator(Some(MAX_EPOLL_SLOTS)).unwrap();
        assert_eq!(ep.event_handlers.len(), 3);
        assert_eq!(ep.dispatch_table.len() as u32, 4 + MAX_EPOLL_SLOTS);
        assert_eq!(allocator.num_slots, MAX_EPOLL_SLOTS as EpollSlot);

        let mut allocator = ep.get_allocator(Some(2)).unwrap();
        let handler = TestEvent::new(true);
        assert!(allocator.allocate(3, Box::new(handler)).is_err());
        let handler = TestEvent::new(true);
        let group = allocator.allocate(2, Box::new(handler)).unwrap();
        let group2 = group.clone();
        assert_eq!(allocator.base().unwrap(), group.base());
        assert_eq!(group.len(), 2);
        assert_eq!(group.len(), group2.len());
        assert_eq!(group.base(), group2.base());

        assert!(allocator.free(group).is_err());
    }

    #[test]
    fn epoll_get_handler() {
        let mut ep = EpollEventMgrVector::new(true, 1);
        let mut allocator = ep.get_allocator(Some(2)).unwrap();
        match ep.get_handler(1) {
            Err(Error::InvalidParameter) => {}
            _ => panic!("handler should be not ready"),
        }

        let handler = TestEvent::new(true);
        let _ = allocator.allocate(2, Box::new(handler)).unwrap();
        let _ = ep.get_handler(0).unwrap();
        let _ = ep.get_handler(0).unwrap();
        assert_eq!(ep.event_handlers.len(), 1);
        assert_eq!(ep.dispatch_table.len(), 2);
    }

    #[test]
    fn epoll_inject_event_test() {
        let mut ep = EpollEventMgrVector::new(true, 1);

        let err = ep.inject_event(0, EpollEvents::EPOLLIN, 0xa5, EpollHandlerPayload::Empty);
        match err {
            Err(Error::SlotOutOfRange(slot)) => {
                assert_eq!(slot, 0);
            }
            _ => panic!("error expected"),
        }

        let handler = TestEvent::new(true);
        let evfd = handler.evfd.try_clone().unwrap();
        let mut allocator = ep.get_allocator(Some(2)).unwrap();
        let _ = allocator.allocate(2, Box::new(handler)).unwrap();
        assert_eq!(ep.event_handlers.len(), 1);
        assert_eq!(ep.dispatch_table.len(), 2);

        assert!(ep
            .inject_event(1, EpollEvents::EPOLLIN, 0xA5, EpollHandlerPayload::Empty)
            .is_ok());
        let val = evfd.read().unwrap();
        assert_eq!(val, 1);

        assert!(ep
            .inject_event(1, EpollEvents::EPOLLIN, 0xA5, EpollHandlerPayload::Empty)
            .is_ok());
        assert!(ep
            .inject_event(2, EpollEvents::EPOLLIN, 0xA5, EpollHandlerPayload::Empty)
            .is_err());
        assert!(ep
            .inject_event(1, EpollEvents::EPOLLIN, 0xA5, EpollHandlerPayload::Empty)
            .is_ok());
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
        let mut allocator = ep.get_allocator(Some(2)).unwrap();
        thread::spawn(move || {
            let fd = handler.evfd.as_raw_fd();
            let group = allocator.allocate(2, Box::new(handler)).unwrap();
            assert_eq!(group.base(), 0);
            assert_eq!(group.len(), 2);
            group
                .register(fd, 2, 0xA5, epoll::Events::EPOLLIN)
                .unwrap_err();
            group
                .deregister(fd, 2, 0xA5, epoll::Events::EPOLLIN)
                .unwrap_err();
            group.register(fd, 1, 0xA5, epoll::Events::EPOLLIN).unwrap();
            group
                .deregister(fd, 1, 0xA5, epoll::Events::EPOLLIN)
                .unwrap();
            group.register(fd, 1, 0xA5, epoll::Events::EPOLLIN).unwrap();
            evfd.write(1).unwrap();
        });
        ep.handle_events(10, -1).unwrap();
    }
}
