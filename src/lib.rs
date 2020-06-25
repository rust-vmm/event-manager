// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

//! Event Manager traits and implementation.
#![deny(missing_docs)]

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::result;
use std::sync::{Arc, Mutex};

use vmm_sys_util::errno::Error as Errno;

mod epoll;
mod events;
mod manager;
mod subscribers;

pub use events::{EventOps, Events};
pub use manager::EventManager;

#[cfg(feature = "remote_endpoint")]
mod endpoint;
#[cfg(feature = "remote_endpoint")]
pub use endpoint::RemoteEndpoint;

/// Error conditions that may appear during `EventManager` related operations.
#[derive(Debug, PartialEq)]
pub enum Error {
    #[cfg(feature = "remote_endpoint")]
    /// Cannot send message on channel.
    ChannelSend,
    #[cfg(feature = "remote_endpoint")]
    /// Cannot receive message on channel.
    ChannelRecv,
    #[cfg(feature = "remote_endpoint")]
    /// Operation on `eventfd` failed.
    EventFd(Errno),
    /// Operation on `libc::epoll` failed.
    Epoll(Errno),
    // TODO: should we allow fds to be registered multiple times?
    /// The fd is already associated with an existing subscriber.
    FdAlreadyRegistered,
    /// The Subscriber ID does not exist or is no longer associated with a Subscriber.
    InvalidId,
}

/// Generic result type that may return `EventManager` errors.
pub type Result<T> = result::Result<T, Error>;

/// Opaque object that uniquely represents a subscriber registered with an `EventManager`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SubscriberId(u64);

impl SubscriberId {
    /// Check whether the SubscriberId is valid.
    ///
    /// SubscriberId 0 is reserved as invalid ID.
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }
}

/// Allows the interaction between an `EventManager` and different event subscribers that do not
/// require a `&mut self` borrow to perform `init` and `process`.
///
/// Any type implementing this also trivially implements `MutEventSubscriber`. The main role of
/// `EventSubscriber` is to allow wrappers such as `Arc` and `Rc` to implement `EventSubscriber`
/// themselves when the inner type is also an implementor.
pub trait EventSubscriber {
    /// Process `events` triggered in the event manager loop.
    ///
    /// Optionally, the subscriber can use `ops` to update the events it monitors.
    fn process(&self, events: Events, ops: &mut EventOps);

    /// Initialization called by the [EventManager](struct.EventManager.html) when the subscriber
    /// is registered.
    ///
    /// The subscriber is expected to use `ops` to register the events it wants to monitor.
    fn init(&self, ops: &mut EventOps);
}

/// Allows the interaction between an `EventManager` and different event subscribers. Methods
/// are invoked with a mutable `self` borrow.
pub trait MutEventSubscriber {
    /// Process `events` triggered in the event manager loop.
    ///
    /// Optionally, the subscriber can use `ops` to update the events it monitors.
    fn process(&mut self, events: Events, ops: &mut EventOps);

    /// Initialization called by the [EventManager](struct.EventManager.html) when the subscriber
    /// is registered.
    ///
    /// The subscriber is expected to use `ops` to register the events it wants to monitor.
    fn init(&mut self, ops: &mut EventOps);
}

/// API that allows users to add, remove, and interact with registered subscribers.
pub trait SubscriberOps {
    /// Subscriber type for which the operations apply.
    type Subscriber: MutEventSubscriber;

    /// Registers a new subscriber and returns the ID associated with it.
    ///
    /// # Panics
    ///
    /// This function might panic if the subscriber is already registered. Whether a panic
    /// is triggered depends on the implementation of
    /// [Subscriber::init()](trait.EventSubscriber.html#tymethod.init).
    ///
    /// Typically, in the `init` function, the subscriber adds fds to its interest list. The same
    /// fd cannot be added twice and the `EventManager` will return
    /// [Error::FdAlreadyRegistered](enum.Error.html). Using `unwrap` in init in this situation
    /// triggers a panic.
    fn add_subscriber(&mut self, subscriber: Self::Subscriber) -> SubscriberId;

    /// Removes the subscriber corresponding to `subscriber_id` from the watch list.
    fn remove_subscriber(&mut self, subscriber_id: SubscriberId) -> Result<Self::Subscriber>;

    /// Returns a mutable reference to the subscriber corresponding to `subscriber_id`.
    fn subscriber_mut(&mut self, subscriber_id: SubscriberId) -> Result<&mut Self::Subscriber>;

    /// Creates an event operations wrapper for the subscriber corresponding to `subscriber_id`.
    ///
    ///  The event operations can be used to update the events monitored by the subscriber.
    fn event_ops(&mut self, subscriber_id: SubscriberId) -> Result<EventOps>;
}

impl<T: EventSubscriber + ?Sized> MutEventSubscriber for T {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        EventSubscriber::process(self, events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        EventSubscriber::init(self, ops);
    }
}

impl MutEventSubscriber for Box<dyn MutEventSubscriber> {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        self.deref_mut().process(events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        self.deref_mut().init(ops);
    }
}

impl MutEventSubscriber for Box<dyn MutEventSubscriber + Send> {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        self.deref_mut().process(events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        self.deref_mut().init(ops);
    }
}

impl MutEventSubscriber for Box<dyn MutEventSubscriber + Sync> {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        self.deref_mut().process(events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        self.deref_mut().init(ops);
    }
}

impl MutEventSubscriber for Box<dyn MutEventSubscriber + Send + Sync> {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        self.deref_mut().process(events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        self.deref_mut().init(ops);
    }
}

impl<T: EventSubscriber + ?Sized> EventSubscriber for Arc<T> {
    fn process(&self, events: Events, ops: &mut EventOps) {
        self.deref().process(events, ops);
    }

    fn init(&self, ops: &mut EventOps) {
        self.deref().init(ops);
    }
}

impl<T: EventSubscriber + ?Sized> EventSubscriber for Rc<T> {
    fn process(&self, events: Events, ops: &mut EventOps) {
        self.deref().process(events, ops);
    }

    fn init(&self, ops: &mut EventOps) {
        self.deref().init(ops);
    }
}

impl<T: MutEventSubscriber + ?Sized> EventSubscriber for RefCell<T> {
    fn process(&self, events: Events, ops: &mut EventOps) {
        self.borrow_mut().process(events, ops);
    }

    fn init(&self, ops: &mut EventOps) {
        self.borrow_mut().init(ops);
    }
}

impl<T: MutEventSubscriber + ?Sized> EventSubscriber for Mutex<T> {
    fn process(&self, events: Events, ops: &mut EventOps) {
        self.lock().unwrap().process(events, ops);
    }

    fn init(&self, ops: &mut EventOps) {
        self.lock().unwrap().init(ops);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscriber_id_derives() {
        let a = SubscriberId(1);
        let b = SubscriberId(1);
        let c = SubscriberId(2);

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(c, b);

        let d = c.clone();
        assert_eq!(c, d);
    }

    #[test]
    fn test_subscriber_id_valid() {
        let mut a = SubscriberId::default();

        assert!(!a.is_valid());
        a.0 = 1;
        assert!(a.is_valid());
        a.0 = 0;
        assert!(!a.is_valid());
        a.0 = 0xffff_ffff_ffff_ffff;
        assert!(a.is_valid());
    }
}
