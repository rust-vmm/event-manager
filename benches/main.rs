// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

use criterion::{criterion_group, criterion_main, Criterion};

use event_manager::{BufferedEventManager, EventManager};
use std::os::fd::AsFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use vmm_sys_util::epoll::EventSet;

// Test the performance of event manager when it manages a single subscriber type.
// The performance is assessed under stress, all added subscribers have active events.
fn run_basic_subscriber(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    let subscribers = (0..no_of_subscribers).map(|_| {
        // Create an eventfd that is initialized with 1 waiting event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let raw_fd = libc::eventfd(1,0);
            assert_ne!(raw_fd, -1);
            OwnedFd::from_raw_fd(raw_fd)
        };

        event_manager.add(event_fd.as_fd(), EventSet::IN | EventSet::ERROR | EventSet::HANG_UP, Box::new(move |_:&mut EventManager<()>, event_set: EventSet| {
            match event_set {
                EventSet::IN => (),
                EventSet::ERROR => {
                    eprintln!("Got error on the monitored event.");
                },
                EventSet::HANG_UP => {
                    panic!("Cannot continue execution. Associated fd was closed.");
                },
                _ => {
                    eprintln!("Received spurious event from the event manager {event_set:#?}.");
                }
            }
        })).unwrap();

        event_fd
    }).collect::<Vec<_>>();

    let n = usize::try_from(no_of_subscribers).unwrap();
    c.bench_function("process_basic", |b| {
        b.iter(|| {
            let mut iter = event_manager.wait(Some(0)).unwrap();
            for _ in 0..n {
                assert_eq!(iter.next(), Some(&mut ()));
            }
            assert_eq!(iter.next(), None);
        })
    });

    drop(subscribers);
}

// Test the performance of event manager when the subscribers are wrapped in an Arc<Mutex>.
// The performance is assessed under stress, all added subscribers have active events.
fn run_arc_mutex_subscriber(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    let subscribers = (0..no_of_subscribers).map(|_| {
        // Create an eventfd that is initialized with 1 waiting event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let raw_fd = libc::eventfd(1,0);
            assert_ne!(raw_fd, -1);
            OwnedFd::from_raw_fd(raw_fd)
        };
        let counter = Arc::new(Mutex::new(0u64));
        let counter_clone = counter.clone();

        event_manager.add(event_fd.as_fd(),EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,Box::new(move |_:&mut EventManager<()>, event_set: EventSet| {
            match event_set {
                EventSet::IN => {
                    *counter_clone.lock().unwrap() += 1;
                },
                EventSet::ERROR => {
                    eprintln!("Got error on the monitored event.");
                },
                EventSet::HANG_UP => {
                    panic!("Cannot continue execution. Associated fd was closed.");
                },
                _ => {
                    eprintln!("Received spurious event from the event manager {event_set:#?}.");
                }
            }
        })).unwrap();

        (event_fd,counter)
    }).collect::<Vec<_>>();

    let n = usize::try_from(no_of_subscribers).unwrap();
    c.bench_function("process_with_arc_mutex", |b| {
        b.iter(|| {
            let mut iter = event_manager.wait(Some(0)).unwrap();
            for _ in 0..n {
                assert_eq!(iter.next(), Some(&mut ()));
            }
            assert_eq!(iter.next(), None);
        })
    });

    drop(subscribers);
}

// Test the performance of event manager when the subscribers are wrapped in an Arc, and they
// leverage inner mutability to update their internal state.
// The performance is assessed under stress, all added subscribers have active events.
fn run_subscriber_with_inner_mut(c: &mut Criterion) {
    let no_of_subscribers = 200i32;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    let subscribers = (0..no_of_subscribers).map(|_| {
        // Create an eventfd that is initialized with 1 waiting event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let raw_fd = libc::eventfd(1,0);
            assert_ne!(raw_fd, -1);
            OwnedFd::from_raw_fd(raw_fd)
        };
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        event_manager.add(event_fd.as_fd(),EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,Box::new(move |_:&mut EventManager<()>, event_set: EventSet| {
            match event_set {
                EventSet::IN => {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                },
                EventSet::ERROR => {
                    eprintln!("Got error on the monitored event.");
                },
                EventSet::HANG_UP => {
                    panic!("Cannot continue execution. Associated fd was closed.");
                },
                _ => {
                    eprintln!("Received spurious event from the event manager {event_set:#?}.");
                }
            }
        })).unwrap();

        (event_fd,counter)
    }).collect::<Vec<_>>();

    let n = usize::try_from(no_of_subscribers).unwrap();
    c.bench_function("process_with_inner_mut", |b| {
        b.iter(|| {
            let mut iter = event_manager.wait(Some(0)).unwrap();
            for _ in 0..n {
                assert_eq!(iter.next(), Some(&mut ()));
            }
            assert_eq!(iter.next(), None);
        })
    });

    drop(subscribers);
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

    let subscribers = (0..no_of_subscribers).map(|_| {
        // Create an eventfd that is initialized with 1 waiting event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let raw_fd = libc::eventfd(1,0);
            assert_ne!(raw_fd, -1);
            OwnedFd::from_raw_fd(raw_fd)
        };
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        event_manager.add(event_fd.as_fd(),EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,Box::new(move |_:&mut EventManager<()>, event_set: EventSet| {
            match event_set {
                EventSet::IN => {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                },
                EventSet::ERROR => {
                    eprintln!("Got error on the monitored event.");
                },
                EventSet::HANG_UP => {
                    panic!("Cannot continue execution. Associated fd was closed.");
                },
                _ => {
                    eprintln!("Received spurious event from the event manager {event_set:#?}.");
                }
            }
        })).unwrap();

        (event_fd,counter)
    }).collect::<Vec<_>>();

    const EVENTS: usize = 3;

    let subscribers_with_data = (0..no_of_subscribers)
        .map(|_| {
            let data = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
            assert_eq!(data.len(), EVENTS);

            // Create eventfd's that are initialized with 1 waiting event.
            let inner_subscribers = (0..EVENTS)
                .map(|_|
                    // SAFETY: Always safe.
                    unsafe {
                        let raw_fd = libc::eventfd(1, 0);
                        assert_ne!(raw_fd, -1);
                        OwnedFd::from_raw_fd(raw_fd)
                    })
                .collect::<Vec<_>>();

            for i in 0..EVENTS {
                let data_clone = data.clone();

                event_manager
                    .add(
                        inner_subscribers[i].as_fd(),
                        EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,
                        Box::new(move |_: &mut EventManager<()>, event_set: EventSet| {
                            match event_set {
                                EventSet::IN => {
                                    data_clone[i].fetch_add(1, Ordering::SeqCst);
                                }
                                EventSet::ERROR => {
                                    eprintln!("Got error on the monitored event.");
                                }
                                EventSet::HANG_UP => {
                                    panic!("Cannot continue execution. Associated fd was closed.");
                                }
                                _ => {}
                            }
                        }),
                    )
                    .unwrap();
            }

            (inner_subscribers, data)
        })
        .collect::<Vec<_>>();

    let n = usize::try_from(total).unwrap();
    c.bench_function("process_dynamic_dispatch", |b| {
        b.iter(|| {
            let mut iter = event_manager.wait(Some(0)).unwrap();
            for _ in 0..n {
                assert_eq!(iter.next(), Some(&mut ()));
            }
            assert_eq!(iter.next(), None);
        })
    });

    drop(subscribers);
    drop(subscribers_with_data);
}

// Test the performance of event manager when it manages a single subscriber type.
// Just a few of the events are active in this test scenario.
fn run_with_few_active_events(c: &mut Criterion) {
    let no_of_subscribers = 200i32;
    let active = 1 + no_of_subscribers / 23;

    let mut event_manager =
        BufferedEventManager::with_capacity(false, no_of_subscribers as usize).unwrap();

    let subscribers = (0..no_of_subscribers).map(|i| {
        // Create an eventfd that is initialized with 1 waiting event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let raw_fd = libc::eventfd((i % 23 == 0) as u8 as u32,0);
            assert_ne!(raw_fd, -1);
            OwnedFd::from_raw_fd(raw_fd)
        };

        event_manager.add(event_fd.as_fd(),EventSet::IN | EventSet::ERROR | EventSet::HANG_UP,Box::new(move |_:&mut EventManager<()>, event_set: EventSet| {
            match event_set {
                EventSet::IN => (),
                EventSet::ERROR => {
                    eprintln!("Got error on the monitored event.");
                },
                EventSet::HANG_UP => {
                    panic!("Cannot continue execution. Associated fd was closed.");
                },
                _ => {
                    eprintln!("Received spurious event from the event manager {event_set:#?}.");
                }
            }
        })).unwrap();

        event_fd
    }).collect::<Vec<_>>();

    let n = usize::try_from(active).unwrap();
    c.bench_function("process_dispatch_few_events", |b| {
        b.iter(|| {
            let mut iter = event_manager.wait(Some(0)).unwrap();
            for _ in 0..n {
                assert_eq!(iter.next(), Some(&mut ()));
            }
            assert_eq!(iter.next(), None);
        })
    });

    drop(subscribers);
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
