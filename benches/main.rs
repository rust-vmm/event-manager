// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

use criterion::{criterion_group, criterion_main, Criterion};

use event_manager::{BufferedEventManager, EventManager};
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use vmm_sys_util::epoll::EventSet;

fn new_eventfd(val: u32) -> OwnedFd {
    // SAFETY: Always safe.
    unsafe {
        let raw_fd = libc::eventfd(val, 0);
        assert_ne!(raw_fd, -1);
        OwnedFd::from_raw_fd(raw_fd)
    }
}

// Test the performance of event manager when it manages a single subscriber type.
// The performance is assessed under stress, all added subscribers have active events.
fn run_basic_subscriber(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    for _ in 0..no_of_subscribers {
        // Create an eventfd that is initialized with 1 waiting event.
        let event_fd = Arc::new(new_eventfd(1));

        event_manager
            .add(
                event_fd,
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => (),
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {
                            eprintln!(
                                "Received spurious event from the event manager {event_set:#?}."
                            );
                        }
                    },
                ),
            )
            .unwrap();
    }

    let expected = vec![(); usize::try_from(no_of_subscribers).unwrap()];
    c.bench_function("process_basic", |b| {
        b.iter(|| {
            assert_eq!(event_manager.wait(Some(0)), Ok(expected.as_slice()));
        })
    });
}

// Test the performance of event manager when the subscribers are wrapped in an Arc<Mutex>.
// The performance is assessed under stress, all added subscribers have active events.
fn run_arc_mutex_subscriber(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    for _ in 0..no_of_subscribers {
        // Create an eventfd that is initialized with 1 waiting event.
        let event_fd = Arc::new(new_eventfd(1));
        let counter = Arc::new(Mutex::new(0u64));

        event_manager
            .add(
                event_fd,
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            *counter.lock().unwrap() += 1;
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {
                            eprintln!(
                                "Received spurious event from the event manager {event_set:#?}."
                            );
                        }
                    },
                ),
            )
            .unwrap();
    }

    let expected = vec![(); usize::try_from(no_of_subscribers).unwrap()];
    c.bench_function("process_with_arc_mutex", |b| {
        b.iter(|| {
            assert_eq!(event_manager.wait(Some(0)), Ok(expected.as_slice()));
        })
    });
}

// Test the performance of event manager when the subscribers are wrapped in an Arc, and they
// leverage inner mutability to update their internal state.
// The performance is assessed under stress, all added subscribers have active events.
fn run_subscriber_with_inner_mut(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    for _ in 0..no_of_subscribers {
        // Create an eventfd that is initialized with 1 waiting event.
        let event_fd = Arc::new(new_eventfd(1));
        let counter = Arc::new(AtomicU64::new(0));

        event_manager
            .add(
                event_fd,
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {
                            eprintln!(
                                "Received spurious event from the event manager {event_set:#?}."
                            );
                        }
                    },
                ),
            )
            .unwrap();
    }

    let expected = vec![(); usize::try_from(no_of_subscribers).unwrap()];
    c.bench_function("process_with_inner_mut", |b| {
        b.iter(|| {
            assert_eq!(event_manager.wait(Some(0)), Ok(expected.as_slice()));
        })
    });
}

// Test the performance of event manager when it manages subscribers of different types, that are
// wrapped in an Arc<Mutex>. Also, make use of `Events` with custom user data
// (using CounterSubscriberWithData).
// The performance is assessed under stress, all added subscribers have active events, and the
// CounterSubscriberWithData subscribers have multiple active events.
fn run_multiple_subscriber_types(c: &mut Criterion) {
    let no_of_subscribers = 100i32;

    let total = no_of_subscribers + (no_of_subscribers * i32::try_from(EVENTS).unwrap());

    let mut event_manager =
        BufferedEventManager::with_capacity(false, usize::try_from(total).unwrap()).unwrap();

    for _ in 0..no_of_subscribers {
        // Create an eventfd that is initialized with 1 waiting event.
        let event_fd = Arc::new(new_eventfd(1));
        let counter = Arc::new(AtomicU64::new(0));

        event_manager
            .add(
                event_fd,
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {
                            eprintln!(
                                "Received spurious event from the event manager {event_set:#?}."
                            );
                        }
                    },
                ),
            )
            .unwrap();
    }

    const EVENTS: usize = 3;

    for _ in 0..no_of_subscribers {
        let data = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
        assert_eq!(data.len(), EVENTS);

        let data_clone = data.clone();
        event_manager
            .add(
                Arc::new(new_eventfd(1)),
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            data_clone[0].fetch_add(1, Ordering::SeqCst);
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {}
                    },
                ),
            )
            .unwrap();
        let data_clone = data.clone();
        event_manager
            .add(
                Arc::new(new_eventfd(1)),
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            data_clone[1].fetch_add(1, Ordering::SeqCst);
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {}
                    },
                ),
            )
            .unwrap();
        let data_clone = data.clone();
        event_manager
            .add(
                Arc::new(new_eventfd(1)),
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => {
                            data_clone[2].fetch_add(1, Ordering::SeqCst);
                        }
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {}
                    },
                ),
            )
            .unwrap();
    }

    let expected = vec![(); usize::try_from(total).unwrap()];
    c.bench_function("process_dynamic_dispatch", |b| {
        b.iter(|| {
            assert_eq!(event_manager.wait(Some(0)), Ok(expected.as_slice()));
        })
    });
}

// Test the performance of event manager when it manages a single subscriber type.
// Just a few of the events are active in this test scenario.
fn run_with_few_active_events(c: &mut Criterion) {
    let no_of_subscribers = 200i32;
    let active = 1 + no_of_subscribers / 23;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    for i in 0..no_of_subscribers {
        // Create an eventfd that is initialized with 1 waiting event.
        let event_fd = new_eventfd((i % 23 == 0) as u8 as u32);
        let event_fd_arc = Arc::new(event_fd);

        event_manager
            .add(
                event_fd_arc.clone(),
                EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                Box::new(
                    move |_: &mut EventManager<()>, event_set: EventSet| match event_set {
                        EventSet::IN => (),
                        EventSet::ERROR => {
                            eprintln!("Got error on the monitored event.");
                        }
                        EventSet::HANG_UP => {
                            panic!("Cannot continue execution. Associated fd was closed.");
                        }
                        _ => {
                            eprintln!(
                                "Received spurious event from the event manager {event_set:#?}."
                            );
                        }
                    },
                ),
            )
            .unwrap();
    }

    let expected = vec![(); usize::try_from(active).unwrap()];
    c.bench_function("process_dispatch_few_events", |b| {
        b.iter(|| {
            assert_eq!(event_manager.wait(Some(0)), Ok(expected.as_slice()));
        })
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(200)
        .measurement_time(std::time::Duration::from_secs(40));
    targets = run_basic_subscriber, run_arc_mutex_subscriber, run_subscriber_with_inner_mut,
        run_multiple_subscriber_types, run_with_few_active_events
);
criterion_main!(benches);
