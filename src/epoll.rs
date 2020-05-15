// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

use std::collections::HashMap;
use std::os::unix::io::RawFd;

use vmm_sys_util::epoll::{ControlOperation, Epoll, EpollEvent};

use super::{Error, EventOps, Result, SubscriberId};

// Internal use structure that keeps the epoll related state of an EventManager.
pub(crate) struct EpollWrapper {
    // The epoll wrapper.
    pub(crate) epoll: Epoll,
    // Records the id of the subscriber associated with the given RawFd. The event event_manager
    // does not currently support more than one subscriber being associated with an fd.
    pub(crate) fd_dispatch: HashMap<RawFd, SubscriberId>,
    // Records the set of fds that are associated with the subscriber that has the given id.
    // This is used to keep track of all fds associated with a subscriber.
    pub(crate) subscriber_watch_list: HashMap<SubscriberId, Vec<RawFd>>,
}

impl EpollWrapper {
    pub(crate) fn new() -> Result<Self> {
        Ok(EpollWrapper {
            epoll: Epoll::new().map_err(Error::Epoll)?,
            fd_dispatch: HashMap::new(),
            subscriber_watch_list: HashMap::new(),
        })
    }

    // Remove the fds associated with the provided subscriber id from the epoll set and the
    // other structures. The subscriber id must be valid.
    pub(crate) fn remove(&mut self, subscriber_id: SubscriberId) {
        let fds = self
            .subscriber_watch_list
            .remove(&subscriber_id)
            .unwrap_or_else(Vec::new);
        for fd in fds {
            // We ignore the result of the operation since there's nothing we can't do, and its
            // not a significant error condition at this point.
            let _ = self
                .epoll
                .ctl(ControlOperation::Delete, fd, EpollEvent::default());
            self.fd_dispatch.remove(&fd);
        }
    }

    // Gets the id of the subscriber associated with the provided fd (if such an association
    // exists).
    pub(crate) fn subscriber_id(&self, fd: RawFd) -> Option<SubscriberId> {
        self.fd_dispatch.get(&fd).copied()
    }

    // Creates and returns a ControlOps object for the subscriber associated with the provided
    // id. The subscriber id must be valid.
    pub(crate) fn ops_unchecked(&mut self, subscriber_id: SubscriberId) -> EventOps {
        EventOps::new(self, subscriber_id)
    }
}
