// Copyright (C) 2019 Alibaba Cloud. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD file.

#![deny(missing_docs)]

//! Traits and Structs to manage and poll IO events by using epoll.
//!
//! The epoll API performs a similar task to poll(2): monitoring multiple file descriptors to see
//! if I/O is possible on any of them. For a typical usage mode, multiple file descriptors are
//! registered on a epoll fd, and a working thread polls and invokes event handler for IO event
//! notifications from the epoll fd.
//!
//! This crate provides IO event management services to its clients by using epoll on Linux.
//! An EpollEventMgrT object manages a epoll fd and a pool of slots. A client may allocate a
//! group of slots from the epoll event manager, and associate an EpollHandler object to those
//! allocated slots. A pair of (fd, epoll_events) may be registered on to each allocated slot.
//! All IO events from the registered fd will be handled by the associated EpollHandler object.

extern crate epoll;

use std::fmt;
use std::fs::File;
use std::os::unix::io::RawFd;

/// Errors for epoll event management operations.
#[derive(Debug)]
pub enum Error {
    /// Invalid parameters.
    InvalidParameter,
    /// Operation not supported.
    OperationNotSupported,
    /// Slot is out of range.
    SlotOutOfRange(EpollSlot),
    /// Too many event slots.
    TooManySlots(EpollSlot),
    /// Event handler not ready.
    HandlerNotReady,
    /// Failure in epoll_ctl().
    EpollCtl(std::io::Error),
    /// Failure in epoll_wait().
    EpollWait(std::io::Error),
    /// Generic IO errors
    IOError(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::InvalidParameter => write!(f, "invalid parameters"),
            Error::OperationNotSupported => write!(f, "operation not supported"),
            Error::SlotOutOfRange(slot) => write!(f, "slot {} is out of range", slot),
            Error::TooManySlots(c) => write!(f, "too many slots {}, max {}", c, MAX_EPOLL_SLOTS),
            Error::HandlerNotReady => write!(f, "event handler not ready"),
            Error::EpollCtl(e) => write!(f, "epoll_ctl(): {}", e),
            Error::EpollWait(e) => write!(f, "epoll_wait(): {}", e),
            Error::IOError(e) => write!(f, "{}", e),
        }
    }
}

/// Maximum number of slots allowed in an allocated group.
pub const MAX_EPOLL_SLOTS: EpollSlot = 256;

/// Slot identifiers to associate file descriptors with registered `EpollHandler` handlers.
pub type EpollSlot = u32;

/// User data associated with an epoll slot.
pub type EpollUserData = u32;

/// Reexported epoll::Events to avoid importing epoll crate to every user.
pub type EpollEvents = epoll::Events;

/// The payload is used to handle events where the internal state of the client needs to be changed.
pub enum EpollHandlerPayload {
    /// DrivePayload(disk_image), a special case inherited from the firecracker project.
    DrivePayload(File),
    /// Events that do not need a payload.
    Empty,
}

/// Data attached to the epoll_event.data field when registering an epoll event.
/// The lower 32-bit contains an EpollSlot, the upper 32-bit contains EpollUserData.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub struct EpollToken(u64);

impl EpollToken {
    /// Create a new EpollToken instance.
    pub fn new(slot: EpollSlot, data: EpollUserData) -> Self {
        EpollToken(u64::from(data) << 32 | u64::from(slot))
    }

    /// Create a new EpollToken instance without user data.
    pub fn from_slot(slot: EpollSlot) -> Self {
        Self::new(slot, 0)
    }

    /// Set epoll slot into the token.
    #[inline]
    pub fn set_slot(&mut self, slot: EpollSlot) {
        self.0 &= !0xFFFF_FFFFu64;
        self.0 |= u64::from(slot);
    }

    /// Get epoll slot from the token.
    #[inline]
    pub fn get_slot(self) -> u32 {
        self.0 as u32
    }

    /// Set user data into the token.
    #[inline]
    pub fn set_user_data(&mut self, data: EpollUserData) {
        self.0 &= 0xFFFF_FFFFu64;
        self.0 |= u64::from(data) << 32;
    }

    /// Get user data from the token.
    #[inline]
    pub fn get_user_data(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

/// Trait to handle epoll events for registered file descriptors.
pub trait EpollHandler: Send {
    /// Type of returned Error.
    ///
    /// This associated type is for errors in the client domain instead of errors from the epoll
    /// manager itself.
    type E: From<Error> + Send + Sync;

    /// Handle IO events for the registered file descriptor.
    fn handle_event(
        &mut self,
        slot: EpollSlot,
        event: EpollEvents,
        data: EpollUserData,
        payload: EpollHandlerPayload,
    ) -> Result<(), Self::E>;

    /// Set or unset the epoll slot group object associated with the handler.
    ///
    /// It will be called during EpollSlotAllocatorT::allocate() with a valid group object,
    /// EpollSlotAllocatorT::allocate() fails if set_group() returns errors.
    /// It will also be called during EpollSlotAllocatorT::free() with null group object
    /// to drop the group object provided during allocated(), and set_group() should never fails
    /// in this case.
    fn set_group(
        &mut self,
        _group: Option<Box<EpollSlotGroupT>>,
    ) -> std::result::Result<(), Self::E> {
        Ok(())
    }
}

/// Trait to manage an epoll fd and a pool of event slots.
pub trait EpollEventMgrT {
    /// Type of event slot allocator.
    type A: EpollSlotAllocatorT<E = Self::E>;
    /// Type of returned Error.
    type E: std::convert::From<Error> + Send + Sync;

    /// Get an epoll slot allocator.
    ///
    /// # Arguments
    /// * num_slots - optional number of event slots to allocate
    ///
    /// When `num_slots` contains a valid number, `num_slots` event slots will be pre-allocated.
    fn get_allocator(&mut self, num_slots: Option<EpollSlot>) -> Result<Self::A, Error>;

    /// Poll events from the epoll fd and invoke registered event handlers.
    ///
    /// # Arguments
    /// * max_events - maximum number of events to handle
    /// * timeout - minimum number of milliseconds that epoll_wait() will block
    ///
    /// Returns a (num_handled_events, num_unknown_events) tuple on success.
    fn handle_events(&mut self, max_events: usize, timeout: i32)
        -> Result<(usize, usize), Self::E>;

    /// Generate a fake epoll event and invoke the registered event handler.
    fn inject_event(
        &mut self,
        slot: EpollSlot,
        events: EpollEvents,
        data: EpollUserData,
        payload: EpollHandlerPayload,
    ) -> Result<(), Self::E>;
}

/// Trait to allocate/free epoll event slots.
pub trait EpollSlotAllocatorT: Send {
    /// Type of allocated event group object.
    type G: EpollSlotGroupT;
    /// Type of returned Error.
    type E: From<Error> + Send + Sync;

    /// Allocate a group of continuous event slots and associated them with `handler`.
    ///
    /// A file may be registered onto each allocated event slot, and IO event notifications
    /// for those file descriptors will be handled by invoking `handler`.
    fn allocate(
        &mut self,
        num_slots: EpollSlot,
        handler: Box<EpollHandler<E = Self::E>>,
    ) -> Result<Self::G, Self::E>;

    /// Free the allocated slots.
    fn free(&mut self, group: Self::G) -> Result<Box<EpollHandler<E = Self::E>>, Self::E>;

    /// Get the base of the pre-allocated slots.
    fn base(&self) -> Option<EpollSlot> {
        None
    }
}

/// A group of epoll slots allocated from the epoll event manager.
///
/// A file descriptor may be associated with each allocated slot to poll specified IO events.
#[allow(clippy::len_without_is_empty)]
pub trait EpollSlotGroupT: Send + Sync {
    /// Get the base of allocated slots.
    fn base(&self) -> EpollSlot;

    /// Get number of epoll slots allocated.
    fn len(&self) -> usize;

    /// Register the target file descriptor `fd` to be polled by the epoll manager.
    ///
    /// # Arguments
    /// * `fd` - File descriptor to be monitored.
    /// * `slot` - index into the allocated slot group.
    /// * `data` - user data associated with the slot.
    /// * `events` - Epoll events to monitor.
    fn register(
        &self,
        fd: RawFd,
        slot: EpollSlot,
        data: EpollUserData,
        events: epoll::Events,
    ) -> Result<(), Error>;

    /// Deregister the target file descriptor `fd` from monitoring.
    ///
    /// # Arguments
    /// * `fd` - File descriptor has been registered.
    /// * `slot` - index into the allocated slot group.
    /// * `data` - user data associated with the slot.
    /// * `events` - Monitored epoll events.
    fn deregister(
        &self,
        fd: RawFd,
        slot: EpollSlot,
        data: EpollUserData,
        events: epoll::Events,
    ) -> Result<(), Error>;
}

/// A simple vector based epoll manager which could only be accessed from a single working thread.
mod vector_single_thread;
pub use vector_single_thread::{
    EpollEventMgrVector as EpollEventMgr, EpollSlotAllocatorVector as EpollSlotAllocator,
    EpollSlotGroupVector as EpollSlotGroup,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token() {
        let mut token = EpollToken::new(1, 1);
        assert_eq!(token, EpollToken(0x1_0000_0001));
        assert_eq!(token.get_slot(), 1);
        assert_eq!(token.get_user_data(), 1);

        token.set_slot(2);
        assert_eq!(token, EpollToken(0x1_0000_0002));
        assert_eq!(token.get_slot(), 2);
        assert_eq!(token.get_user_data(), 1);

        token.set_user_data(3);
        assert_eq!(token, EpollToken(0x3_0000_0002));
        assert_eq!(token.get_slot(), 2);
        assert_eq!(token.get_user_data(), 3);
    }
}
