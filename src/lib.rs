// Copyright (C) 2019 Alibaba Cloud. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD file.

#![deny(missing_docs)]

//! Traits to manage and poll IO event notifications for registered file decriptors by using epoll.
//!
//! The epoll API performs a similar task to poll(2): monitoring multiple file descriptors to see
//! if I/O is possible on any of them. For a typical usage mode, multiple file descriptors are
//! registered on a epoll fd, and a working thread polls and invokes event handler for IO event
//! notifications from the epoll fd.
//!
//! Several traits are defined to support the above usage case:
//! - EpollEventMgr: manages a epoll fd and a pool of event slots. One file descriptor/event handler
//! could be registered onto each slot.
//! - EpollSlotAllocator: allocates slots from the epoll event manager.
//! - EpollSlotGroup: a group of slots allocated from the epoll event manager.

use std::fmt;
use std::fs::File;
use std::os::unix::io::RawFd;

/// Errors for epoll event manager.
#[derive(Debug)]
pub enum Error {
    /// Invalid parameters.
    InvalidParameter,
    /// Operation not supported.
    OperationNotSupported,
    /// Slot is out of range.
    SlotOutOfRange(EpollSlot),
    /// Too many event slots.
    TooManySlots(usize),
    /// Event handler not ready.
    HandlerNotReady,
    /// Failure in epoll_ctl().
    EpollCtl(std::io::Error),
    /// Failure in epoll_wait().
    EpollWait(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::InvalidParameter => write!(f, "invalid parameters"),
            Error::OperationNotSupported => write!(f, "operation not supported"),
            Error::SlotOutOfRange(slot) => write!(f, "slot {} is out of range", slot),
            Error::TooManySlots(c) => write!(f, "too many slots {}, max {}", c, MAX_EPOLL_SLOTS),
            Error::HandlerNotReady => write!(f, "event handler not ready"),
            Error::EpollCtl(e) => write!(f, "failure in epoll_ctl(): {}", e),
            Error::EpollWait(e) => write!(f, "failure in epoll_wait(): {}", e),
        }
    }
}

/// Result for epoll event management operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Maximum number of slots allowed for an allocation.
pub const MAX_EPOLL_SLOTS: usize = 256;

/// Epoll slot to associate a file descriptor with a registered `EpollHandler` handler.
pub type EpollSlot = u64;

/// The payload is used to handle events where the internal state of the VirtIO device
/// needs to be changed.
pub enum EpollHandlerPayload {
    /// DrivePayload(disk_image)
    DrivePayload(File),
    /// Events that do not need a payload.
    Empty,
}

/// Trait to handle epoll events for registered file descriptors.
pub trait EpollHandler: Send {
    /// Handle io events associated with the slot.
    fn handle_event(
        &mut self,
        slot: EpollSlot,
        event_flags: u32,
        payload: EpollHandlerPayload,
    ) -> Result<()>;
}

/// Trait to manage an epoll fd and a pool of event slots.
trait EpollEventMgr {
    /// Type of event slot allocator.
    type A: EpollSlotAllocator;

    /// Get an epoll slot allocator.
    ///
    /// # Arguments
    /// * num_slots - number of event slots to allocate
    /// * handler - event handler to associated with the allocated event slots
    fn get_allocator(
        &mut self,
        num_slots: Option<usize>,
        handler: Option<Box<EpollHandler>>,
    ) -> Result<Self::A>;

    /// Poll events from the epoll fd and invoke registered event handlers.
    ///
    /// # Arguments
    /// * max_events - maximum number of events to handle
    /// * timeout - minimum number of milliseconds that epoll_wait() will block
    ///
    /// Returns a (num_handled_events, num_unknown_events) tuple on success.
    fn handle_events(&mut self, max_events: usize, timeout: i32) -> Result<(usize, usize)>;

    /// Generate a fake epoll event and invoke the registered event handler.
    fn inject_event(
        &mut self,
        slot: EpollSlot,
        event_flags: u32,
        payload: EpollHandlerPayload,
    ) -> Result<()>;
}

/// Trait to allocate/free epoll event slots.
pub trait EpollSlotAllocator {
    /// Type of allocated event group object.
    type G: EpollSlotGroup;

    /// Allocate a group of event slots and associated them with `handler`.
    ///
    /// A file may be registered onto each allocated event slot, and IO event notifications
    /// for those file descriptors will be handled by invoking `handler`.
    fn allocate(&mut self, num_slots: usize, handler: Box<EpollHandler>) -> Result<Self::G>;

    /// Free the allocated slots.
    fn free(&mut self, group: &mut Self::G) -> Result<()>;
}

/// A group of event slots allocated from the epoll event manager.
///
/// A file descriptor may be associated for each allocated slot to poll specified epoll events.
#[allow(clippy::len_without_is_empty)]
pub trait EpollSlotGroup {
    /// Get number of epoll slots allocated.
    fn len(&self) -> usize;

    /// Register the target file descriptor `fd` to be polled by the epoll manager.
    ///
    /// # Arguments
    /// * `fd` - File descriptor to be monitored.
    /// * `slot` -  an allocated epoll slot to be associated with `fd`.
    /// * `events` - Epoll events to monitor.
    fn register(&self, fd: RawFd, slot: EpollSlot, events: epoll::Events) -> Result<()>;

    /// Deregister the target file descriptor `fd` from monitoring.
    ///
    /// # Arguments
    /// * `fd` - File descriptor has been registered.
    /// * `slot` -  an allocated epoll slot associated with `fd`.
    /// * `events` - Monitored epoll events.
    fn deregister(&self, fd: RawFd, slot: EpollSlot, events: epoll::Events) -> Result<()>;
}

mod vector_single_thread;
pub use vector_single_thread::{
    EpollEventMgrVector, EpollSlotAllocatorVector, EpollSlotGroupVector,
};
