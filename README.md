# event-manager

[![crates.io](https://img.shields.io/crates/v/event-manager)](https://crates.io/crates/event-manager)
[![docs.rs](https://img.shields.io/docsrs/event-manager)](https://docs.rs/event-manager/)

The `event-manager` provides abstractions for implementing event based
systems. For now, this crate only works on Linux and uses the
[epoll](http://man7.org/linux/man-pages/man7/epoll.7.html) API to provide a
mechanism for handling I/O notifications.

## Design

This crate offers an abstraction (`EventManager`) over [epoll](https://man7.org/linux/man-pages/man7/epoll.7.html) that
allows for more ergonomic usage with many file descriptors.

The `EventManager` allows adding and removing file descriptors with a callback closure. The
`EventManager` interest list can also be modified within these callback closures.

A typical event-based application:

1. Creates the `EventManager` (`EventManager::default()`).
2. Registers file descriptors with (`EventManager::add`).
3. Calls `EventManager::wait` in a loop.

Read more in the [design document](docs/DESIGN.md).

## Implementing an event

Like `epoll` a file descriptor only monitors specific events.

The events ars specified when calling `EventManager::add` with `vmm_sys_util::epoll::EventSet`.

When an event becomes ready, the event manager will call the file descriptors callback closure.

The `epoll` events `EPOLLRDHUP`, `EPOLLERR` and `EPOLLHUP` (which correspond to
`EventSet::READ_HANG_UP`, `EventSet::ERROR` and `EventSet::HANG_UP` respectively) are documented to
always report, even when not specified by the user.

> epoll_wait(2) will always report for this event; it is not
> necessary to set it in events when calling epoll_ctl().

*https://man7.org/linux/man-pages/man2/epoll_ctl.2.html*

As such it is best practice to always handle the cases where the `EventSet` passed to the file
descriptor callback closure is `EventSet::READ_HANG_UP`, `EventSet::ERROR` or `EventSet::HANG_UP`.

## Development and Testing

`event-manager` uses [`rust-vmm-ci`](https://github.com/rust-vmm/rust-vmm-ci) for continuous
testing. All tests are run in the `rustvmm/dev` container.

More details on running the tests can be found in the
[development](docs/DEVELOPMENT.md) document.

## License

This project is licensed under either of:

- [Apache License](LICENSE-APACHE), Version 2.0
- [BSD-3-Clause License](LICENSE-BSD-3-CLAUSE)
