// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Event Manager traits and implementation.
#![deny(missing_docs)]
use std::io;
use std::result;

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
#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "remote_endpoint")]
    /// Cannot send message on channel.
    ChannelSend,
    #[cfg(feature = "remote_endpoint")]
    /// Cannot receive message on channel.
    ChannelRecv,
    #[cfg(feature = "remote_endpoint")]
    /// Operation on `eventfd` failed.
    EventFd(io::Error),
    /// Operation on `libc::epoll` failed.
    Epoll(io::Error),
    // TODO: should we allow fds to be registered multiple times?
    /// The fd is already associated with an existing subscriber.
    FdAlreadyRegistered,
    /// The Subscriber ID does not exist or is no longer associated with a Subscriber.
    InvalidId,
}

/// Generic result type that may return `EventManager` errors.
pub type Result<T> = result::Result<T, Error>;

/// Opaque object that uniquely represents a subscriber registered with an `EventManager`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SubscriberId(u64);

/// Allows the interaction between an `EventManager` and different event subscribers.
pub trait EventSubscriber {
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
    type Subscriber: EventSubscriber;

    /// Registers a new subscriber and returns the ID associated with it.
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
