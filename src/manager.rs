// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

use std::sync::{Arc, Mutex};

use vmm_sys_util::epoll::EpollEvent;
#[cfg(feature = "remote_endpoint")]
use vmm_sys_util::epoll::{ControlOperation, EventSet};

#[cfg(feature = "remote_endpoint")]
use super::endpoint::{EventManagerChannel, RemoteEndpoint};
use super::epoll::EpollWrapper;
use super::subscribers::Subscribers;
use super::{Error, EventOps, EventSubscriber, Events, Result, SubscriberId, SubscriberOps};

/// Allows event subscribers to be registered, connected to the event loop, and later removed.
pub struct EventManager<T> {
    subscribers: Subscribers<T>,
    epoll_context: EpollWrapper,
    ready_events: Vec<EpollEvent>,

    #[cfg(feature = "remote_endpoint")]
    channel: EventManagerChannel<T>,
}

impl<T: EventSubscriber> SubscriberOps for EventManager<T> {
    type Subscriber = T;

    /// Register a subscriber with the event event_manager and returns the associated ID.
    fn add_subscriber(&mut self, subscriber: T) -> SubscriberId {
        let subscriber_id = self.subscribers.add(subscriber);
        self.subscribers
            .get_mut_unchecked(subscriber_id)
            // The index is valid because we've just added the subscriber.
            .init(&mut self.epoll_context.ops_unchecked(subscriber_id));
        subscriber_id
    }

    /// Unregisters and returns the subscriber associated with the provided ID.
    fn remove_subscriber(&mut self, subscriber_id: SubscriberId) -> Result<T> {
        let subscriber = self
            .subscribers
            .remove(subscriber_id)
            .ok_or(Error::InvalidId)?;
        self.epoll_context.remove(subscriber_id);
        Ok(subscriber)
    }

    /// Return a mutable reference to the subscriber associated with the provided id.
    fn subscriber_mut(&mut self, subscriber_id: SubscriberId) -> Result<&mut T> {
        if self.subscribers.contains(subscriber_id) {
            return Ok(self.subscribers.get_mut_unchecked(subscriber_id));
        }
        Err(Error::InvalidId)
    }

    /// Returns a `ControlOps` object for the subscriber associated with the provided ID.
    fn event_ops(&mut self, subscriber_id: SubscriberId) -> Result<EventOps> {
        // Check if the subscriber_id is valid.
        if self.subscribers.contains(subscriber_id) {
            // The index is valid because the result of `find` was not `None`.
            return Ok(self.epoll_context.ops_unchecked(subscriber_id));
        }
        Err(Error::InvalidId)
    }
}

// TODO: add implementations for other standard wrappers as well.
impl EventSubscriber for Arc<Mutex<dyn EventSubscriber>> {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        self.lock().unwrap().process(events, ops);
    }

    fn init(&mut self, ops: &mut EventOps) {
        self.lock().unwrap().init(ops);
    }
}

impl<S: EventSubscriber> EventManager<S> {
    /// Create a new `EventManger` object.
    pub fn new() -> Result<Self> {
        let manager = EventManager {
            subscribers: Subscribers::new(),
            epoll_context: EpollWrapper::new()?,
            ready_events: vec![EpollEvent::default(); 256],
            #[cfg(feature = "remote_endpoint")]
            channel: EventManagerChannel::new()?,
        };

        #[cfg(feature = "remote_endpoint")]
        manager
            .epoll_context
            .epoll
            .ctl(
                ControlOperation::Add,
                manager.channel.fd(),
                EpollEvent::new(EventSet::IN, manager.channel.fd() as u64),
            )
            .map_err(Error::Epoll)?;
        Ok(manager)
    }

    /// Run the event loop blocking until events are triggered or an error is returned.
    /// Calls [subscriber.process()](trait.EventSubscriber.html#tymethod.process) for each event.
    ///
    /// On success, it returns number of dispatched events or 0 when interrupted by a signal.
    pub fn run(&mut self) -> Result<usize> {
        self.run_with_timeout(-1)
    }

    /// Wait for events for a maximum timeout of `miliseconds` or until an error is returned.
    /// Calls [subscriber.process()](trait.EventSubscriber.html#tymethod.process) for each event.
    ///
    /// On success, it returns number of dispatched events or 0 when interrupted by a signal.
    pub fn run_with_timeout(&mut self, milliseconds: i32) -> Result<usize> {
        let event_count = match self.epoll_context.epoll.wait(
            self.ready_events.len(),
            milliseconds,
            &mut self.ready_events[..],
        ) {
            Ok(ev) => ev,
            // EINTR is not actually an error that needs to be handled. The documentation
            // for epoll.run specifies that run exits when it for an event, on timeout, or
            // on interrupt.
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => return Ok(0),
            Err(e) => return Err(Error::Epoll(e)),
        };
        self.dispatch_events(event_count);

        Ok(event_count)
    }

    fn dispatch_events(&mut self, event_count: usize) {
        // Use the temporary, pre-allocated buffer to check ready events.
        for ev_index in 0..event_count {
            let event = self.ready_events[ev_index];

            #[cfg(feature = "remote_endpoint")]
            {
                if self.dispatch_endpoint_event(event) {
                    continue;
                }
            }

            let fd = event.fd();
            // This error condition can happen when the fd associated with a subscriber was closed,
            // and the subscriber did not handle the RHUP/
            let subscriber_id = self
                .epoll_context
                .subscriber_id(fd)
                .expect("Received event on fd from subscriber that does not exist");

            self.subscribers.get_mut_unchecked(subscriber_id).process(
                Events::with_inner(event),
                // The `subscriber_id` is valid because we checked it before.
                &mut self.epoll_context.ops_unchecked(subscriber_id),
            );
        }
    }
}

#[cfg(feature = "remote_endpoint")]
impl<S: EventSubscriber> EventManager<S> {
    /// Return a `RemoteEndpoint` object, that allows interacting with the `EventManager` from a
    /// different thread. Using `RemoteEndpoint::call_blocking` on the same thread the event loop
    /// runs on leads to a deadlock.
    pub fn remote_endpoint(&self) -> RemoteEndpoint<S> {
        self.channel.remote_endpoint()
    }

    // Returns true if there are any endpoints to be dispatched.
    fn dispatch_endpoint_event(&mut self, event: EpollEvent) -> bool {
        if event.fd() == self.channel.fd() {
            if event.event_set() != EventSet::IN {
                // This situation is virtually impossible to occur. If it does we have
                // a programming error in our code.
                unreachable!();
            }
            self.handle_endpoint_calls();
            return true;
        }
        false
    }

    fn handle_endpoint_calls(&mut self) {
        // Clear the inner event_fd. We don't do anything about an error here at this point.
        let _ = self.channel.event_fd.read();

        // Process messages. We consider only `Empty` errors can appear here; we don't check
        // for `Disconnected` errors because we keep at least one clone of `channel.sender` alive
        // at all times ourselves.
        while let Ok(msg) = self.channel.receiver.try_recv() {
            // We call the inner closure and attempt to send back the result, but can't really do
            // anything in case of error here.
            let _ = msg.sender.send((msg.fnbox)(self));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::io::{AsRawFd, RawFd};
    use vmm_sys_util::{epoll::EventSet, eventfd::EventFd};

    struct DummySubscriber {
        event_fd_1: EventFd,
        event_fd_2: EventFd,

        // Flags used for checking that the event event_manager called the `process`
        // function for ev1/ev2.
        processed_ev1_out: bool,
        processed_ev2_out: bool,
        processed_ev1_in: bool,

        // Flags used for driving register/unregister/modify of events from
        // outside of the `process` function.
        register_ev2: bool,
        unregister_ev1: bool,
        modify_ev1: bool,
    }

    impl DummySubscriber {
        fn new() -> Self {
            DummySubscriber {
                event_fd_1: EventFd::new(0).unwrap(),
                event_fd_2: EventFd::new(0).unwrap(),
                processed_ev1_out: false,
                processed_ev2_out: false,
                processed_ev1_in: false,
                register_ev2: false,
                unregister_ev1: false,
                modify_ev1: false,
            }
        }
    }

    impl DummySubscriber {
        fn register_ev2(&mut self) {
            self.register_ev2 = true;
        }

        fn unregister_ev1(&mut self) {
            self.unregister_ev1 = true;
        }

        fn modify_ev1(&mut self) {
            self.modify_ev1 = true;
        }

        fn processed_ev1_out(&self) -> bool {
            self.processed_ev1_out
        }

        fn processed_ev2_out(&self) -> bool {
            self.processed_ev2_out
        }

        fn processed_ev1_in(&self) -> bool {
            self.processed_ev1_in
        }

        fn reset_state(&mut self) {
            self.processed_ev1_out = false;
            self.processed_ev2_out = false;
            self.processed_ev1_in = false;
        }

        fn handle_updates(&mut self, event_manager: &mut EventOps) {
            if self.register_ev2 {
                event_manager
                    .add(Events::new(&self.event_fd_2, EventSet::OUT))
                    .unwrap();
                self.register_ev2 = false;
            }

            if self.unregister_ev1 {
                event_manager
                    .remove(Events::new_raw(
                        self.event_fd_1.as_raw_fd(),
                        EventSet::empty(),
                    ))
                    .unwrap();
                self.unregister_ev1 = false;
            }

            if self.modify_ev1 {
                event_manager
                    .modify(Events::new(&self.event_fd_1, EventSet::IN))
                    .unwrap();
                self.modify_ev1 = false;
            }
        }

        fn handle_in(&mut self, source: RawFd) {
            if self.event_fd_1.as_raw_fd() == source {
                self.processed_ev1_in = true;
            }
        }

        fn handle_out(&mut self, source: RawFd) {
            match source {
                _ if self.event_fd_1.as_raw_fd() == source => {
                    self.processed_ev1_out = true;
                }
                _ if self.event_fd_2.as_raw_fd() == source => {
                    self.processed_ev2_out = true;
                }
                _ => {}
            }
        }
    }

    impl EventSubscriber for DummySubscriber {
        fn process(&mut self, events: Events, ops: &mut EventOps) {
            let source = events.fd();
            let event_set = events.event_set();

            // We only know how to treat EPOLLOUT and EPOLLIN.
            // If we received anything else just stop processing the event.
            let all_but_in_out = EventSet::all() - EventSet::OUT - EventSet::IN;
            if event_set.intersects(all_but_in_out) {
                return;
            }

            self.handle_updates(ops);

            match event_set {
                EventSet::IN => self.handle_in(source),
                EventSet::OUT => self.handle_out(source),
                _ => {}
            }
        }

        fn init(&mut self, ops: &mut EventOps) {
            let event = Events::new(&self.event_fd_1, EventSet::OUT);
            ops.add(event).unwrap();
        }
    }

    #[test]
    fn test_register() {
        use super::SubscriberOps;

        let mut event_manager = EventManager::<Arc<Mutex<dyn EventSubscriber>>>::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager.add_subscriber(dummy_subscriber.clone());

        dummy_subscriber.lock().unwrap().register_ev2();

        // When running the loop the first time, ev1 should be processed, but ev2 shouldn't
        // because it was just added as part of processing ev1.
        event_manager.run().unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), true);
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev2_out(), false);

        // Check that both ev1 and ev2 are processed.
        dummy_subscriber.lock().unwrap().reset_state();
        event_manager.run().unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), true);
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev2_out(), true);
    }

    #[test]
    #[should_panic(expected = "FdAlreadyRegistered")]
    fn test_add_invalid_subscriber() {
        use std::os::unix::io::FromRawFd;

        let mut event_manager = EventManager::<Arc<Mutex<dyn EventSubscriber>>>::new().unwrap();
        let subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager.add_subscriber(subscriber.clone());

        // Create a subscriber with the same registered event as an existing subscriber.
        let invalid_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));
        invalid_subscriber.lock().unwrap().event_fd_1 = unsafe {
            EventFd::from_raw_fd(subscriber.lock().unwrap().event_fd_1.as_raw_fd() as RawFd)
        };

        // This call will generate a panic coming from the way init() on DummySubscriber
        // is implemented. In a production setup, unwraps should probably not be used.
        event_manager.add_subscriber(invalid_subscriber);
    }

    // Test that unregistering an event while processing another one works.
    #[test]
    fn test_unregister() {
        let mut event_manager = EventManager::<Arc<Mutex<dyn EventSubscriber>>>::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager.add_subscriber(dummy_subscriber.clone());

        // Disable ev1. We should only receive this event once.
        dummy_subscriber.lock().unwrap().unregister_ev1();

        event_manager.run().unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), true);

        dummy_subscriber.lock().unwrap().reset_state();

        // We expect no events to be available. Let's run with timeout so that run exists.
        event_manager.run_with_timeout(100).unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), false);
    }

    #[test]
    fn test_modify() {
        let mut event_manager = EventManager::<Arc<Mutex<dyn EventSubscriber>>>::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager.add_subscriber(dummy_subscriber.clone());

        // Modify ev1 so that it waits for EPOLL_IN.
        dummy_subscriber.lock().unwrap().modify_ev1();
        event_manager.run().unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), true);
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev2_out(), false);

        dummy_subscriber.lock().unwrap().reset_state();

        // Make sure ev1 is ready for IN so that we don't loop forever.
        dummy_subscriber
            .lock()
            .unwrap()
            .event_fd_1
            .write(1)
            .unwrap();

        event_manager.run().unwrap();
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_out(), false);
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev2_out(), false);
        assert_eq!(dummy_subscriber.lock().unwrap().processed_ev1_in(), true);
    }

    #[test]
    #[cfg(feature = "remote_endpoint")]
    fn test_endpoint() {
        use std::thread;

        let mut event_manager = EventManager::<DummySubscriber>::new().unwrap();
        let dummy = DummySubscriber::new();
        let endpoint = event_manager.remote_endpoint();

        let thread_handle = thread::spawn(move || {
            event_manager.run().unwrap();
        });

        dummy.event_fd_1.write(1).unwrap();

        let token = endpoint
            .call_blocking(|sub_ops| -> Result<SubscriberId> { Ok(sub_ops.add_subscriber(dummy)) })
            .unwrap();

        assert_eq!(token, SubscriberId(1));

        thread_handle.join().unwrap();
    }
}
