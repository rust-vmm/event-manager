#![warn(missing_debug_implementations)]

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use vmm_sys_util::epoll::EventSet;

/// The function thats runs when an event occurs.
type Action<T> = Box<dyn Fn(&mut EventManager<T>, EventSet) -> T>;

fn errno() -> i32 {
    // SAFETY: Always safe.
    unsafe { *libc::__errno_location() }
}

#[derive(Debug)]
pub struct BufferedEventManager<T> {
    event_manager: EventManager<T>,
    // TODO The length is always unused, a custom type could thus save `size_of::<usize>()` bytes.
    buffer: Vec<libc::epoll_event>,
    // TODO The length is always unused, a custom type could thus save `size_of::<usize>()` bytes.
    output_buffer: Vec<T>,
}

impl<T> BufferedEventManager<T> {
    /// Returns a reference to the inner epoll file descriptor.
    pub fn epfd(&self) -> BorrowedFd {
        self.event_manager.epfd.as_fd()
    }

    /// Add an entry to the interest list of the epoll file descriptor.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_ctl`] returns `-1`.
    pub fn add<Fd: AsRawFd>(&mut self, fd: Fd, events: EventSet, f: Action<T>) -> Result<(), i32> {
        let res = self.event_manager.add(fd, events, f);
        self.buffer.reserve(self.event_manager.events.len());
        self.output_buffer.reserve(self.event_manager.events.len());
        res
    }

    /// Remove (deregister) the target file descriptor fd from the interest list.
    ///
    /// Returns `Ok(true)` when the given `fd` was present and `Ok(false)` when it wasn't.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_ctl`] returns `-1`.
    pub fn del<Fd: AsRawFd>(&mut self, fd: Fd) -> Result<bool, i32> {
        self.event_manager.del(fd)
    }

    /// Waits until an event fires then triggers the respective action returning `Ok(x)`. If
    /// timeout is `Some(_)` it may also return after the given number of milliseconds with
    /// `Ok(0)`.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_wait`] returns `-1`.
    ///
    /// # Panics
    ///
    /// When the value given in timeout does not fit within an `i32` e.g.
    /// `timeout.map(|u| i32::try_from(u).unwrap())`.
    pub fn wait(&mut self, timeout: Option<u32>) -> Result<&[T], i32> {
        // SAFETY: `EventManager::wait` initializes N element from the start of the slice and only
        // accesses these, thus it will never access uninitialized memory, making this safe.
        unsafe {
            self.buffer.set_len(self.buffer.capacity());
            self.output_buffer.set_len(self.output_buffer.capacity());
        }
        let n = self
            .event_manager
            .wait(timeout, &mut self.buffer, &mut self.output_buffer)?;
        // SAFETY: This is safe as we call `epoll_wait` within `self.event_manager.wait` with
        // `self.buffer.len()` which ensures `n` will be less than or equal to `self.buffer.len()`
        // which ensures this slice will only cover valid elements.
        unsafe {
            Ok(self
                .output_buffer
                .get_unchecked(..usize::try_from(n).unwrap_unchecked()))
        }
    }

    /// Creates new event manager.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_create1`] returns `-1`.
    pub fn new(close_exec: bool) -> Result<Self, i32> {
        Ok(BufferedEventManager {
            event_manager: EventManager::new(close_exec)?,
            buffer: Vec::with_capacity(0),
            output_buffer: Vec::with_capacity(0),
        })
    }
    pub fn with_capacity(close_exec: bool, capacity: usize) -> Result<Self, i32> {
        Ok(BufferedEventManager {
            event_manager: EventManager::new(close_exec)?,
            buffer: Vec::with_capacity(capacity),
            output_buffer: Vec::with_capacity(capacity),
        })
    }
}

impl<T> Default for BufferedEventManager<T> {
    fn default() -> Self {
        Self::new(false).unwrap()
    }
}

pub struct EventManager<T> {
    epfd: OwnedFd,
    events: HashMap<RawFd, Action<T>>,
}

impl<T> std::fmt::Debug for EventManager<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventManager")
            .field("epfd", &self.epfd)
            .field(
                "events",
                &self
                    .events
                    .iter()
                    .map(|(k, v)| (*k, v as *const _ as usize))
                    .collect::<HashMap<_, _>>(),
            )
            .finish()
    }
}

impl<T> EventManager<T> {
    /// Returns a reference to the inner epoll file descriptor.
    pub fn epfd(&self) -> BorrowedFd {
        self.epfd.as_fd()
    }

    /// Add an entry to the interest list of the epoll file descriptor.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_ctl`] returns `-1`.
    pub fn add<Fd: AsRawFd>(&mut self, fd: Fd, events: EventSet, f: Action<T>) -> Result<(), i32> {
        let mut event = libc::epoll_event {
            events: events.bits(),
            r#u64: u64::try_from(fd.as_raw_fd()).unwrap(),
        };
        // SAFETY: Safe when `fd` is a valid file descriptor.
        match unsafe {
            libc::epoll_ctl(
                self.epfd.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                fd.as_raw_fd(),
                &mut event,
            )
        } {
            0 => {
                self.events.insert(fd.as_raw_fd(), f);
                Ok(())
            }
            -1 => Err(errno()),
            _ => unreachable!(),
        }
    }

    /// Remove (deregister) the target file descriptor fd from the interest list.
    ///
    /// Returns `Ok(true)` when the given `fd` was present and `Ok(false)` when it wasn't.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_ctl`] returns `-1`.
    pub fn del<Fd: AsRawFd>(&mut self, fd: Fd) -> Result<bool, i32> {
        match self.events.remove(&fd.as_raw_fd()) {
            Some(_) => {
                // SAFETY: Safe when `fd` is a valid file descriptor.
                match unsafe {
                    libc::epoll_ctl(
                        self.epfd.as_raw_fd(),
                        libc::EPOLL_CTL_DEL,
                        fd.as_raw_fd(),
                        std::ptr::null_mut(),
                    )
                } {
                    0 => Ok(true),
                    -1 => Err(errno()),
                    _ => unreachable!(),
                }
            }
            None => Ok(false),
        }
    }

    /// Waits until an event fires then triggers the respective action returning `Ok(x)`. If
    /// timeout is `Some(_)` it may also return after the given number of milliseconds with
    /// `Ok(0)`.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_wait`] returns `-1`.
    ///
    /// # Panics
    ///
    /// When the value given in timeout does not fit within an `i32` e.g.
    /// `timeout.map(|u| i32::try_from(u).unwrap())`.
    pub fn wait(
        &mut self,
        timeout: Option<u32>,
        buffer: &mut [libc::epoll_event],
        output_buffer: &mut [T],
    ) -> Result<i32, i32> {
        // SAFETY: Always safe.
        match unsafe {
            libc::epoll_wait(
                self.epfd.as_raw_fd(),
                buffer.as_mut_ptr(),
                buffer.len().try_into().unwrap_unchecked(),
                timeout.map_or(-1i32, |u| i32::try_from(u).unwrap()),
            )
        } {
            -1 => Err(errno()),
            // SAFETY: `x` elements are initialized by `libc::epoll_wait`.
            n @ 0.. => unsafe {
                #[allow(clippy::needless_range_loop)]
                for i in 0..usize::try_from(n).unwrap_unchecked() {
                    let event = buffer[i];
                    // For all events which can fire there exists an entry within `self.events` thus
                    // it is safe to unwrap here.
                    let f: *const dyn Fn(&mut EventManager<T>, EventSet) -> T = self
                        .events
                        .get(&i32::try_from(event.u64).unwrap_unchecked())
                        .unwrap_unchecked();
                    output_buffer[i] = (*f)(self, EventSet::from_bits_unchecked(event.events));
                }
                Ok(n)
            },
            _ => unreachable!(),
        }
    }

    /// Creates new event manager.
    ///
    /// # Errors
    ///
    /// When [`libc::epoll_create1`] returns `-1`.
    pub fn new(close_exec: bool) -> Result<Self, i32> {
        // SAFETY: Always safe.
        match unsafe { libc::epoll_create1(if close_exec { libc::EPOLL_CLOEXEC } else { 0 }) } {
            -1 => Err(errno()),
            epfd => Ok(Self {
                // SAFETY: Always safe.
                epfd: unsafe { OwnedFd::from_raw_fd(epfd) },
                events: HashMap::new(),
            }),
        }
    }
}

impl<T> Default for EventManager<T> {
    fn default() -> Self {
        Self::new(false).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    #[test]
    fn debug() {
        let manager = BufferedEventManager::<()>::default();
        let epfd = manager.epfd().as_raw_fd();
        assert_eq!(format!("{manager:?}"),format!("BufferedEventManager {{ event_manager: EventManager {{ epfd: OwnedFd {{ fd: {epfd} }}, events: {{}} }}, buffer: [], output_buffer: [] }}"));
    }

    #[test]
    fn del_none() {
        let mut manager = BufferedEventManager::<()>::with_capacity(false, 10).unwrap();
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let fd = libc::eventfd(1, 0);
            assert_ne!(fd, -1);
            fd
        };
        assert_eq!(manager.del(event_fd), Ok(false));

        // SAFETY: `event_fd` is a valid file descriptor.
        unsafe { libc::close(event_fd) };
    }

    #[test]
    fn delete() {
        static COUNT: AtomicBool = AtomicBool::new(false);
        let mut manager = BufferedEventManager::default();

        // We set value to 1 so it will trigger on a read event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let fd = libc::eventfd(1, 0);
            assert_ne!(fd, -1);
            fd
        };

        manager
            .add(
                event_fd,
                EventSet::IN,
                // A closure which will flip the atomic boolean then remove the event fd from the
                // interest list.
                Box::new(move |x: &mut EventManager<()>, _| {
                    // Flips the atomic.
                    let cur = COUNT.load(Ordering::SeqCst);
                    COUNT.store(!cur, Ordering::SeqCst);
                    // Calls `EventManager::del` which removes the target file descriptor fd from
                    // the interest list of the inner epoll.
                    x.del(event_fd).unwrap();
                }),
            )
            .unwrap();

        // Assert the initial state of the atomic boolean.
        assert!(!COUNT.load(Ordering::SeqCst));

        // The file descriptor has been pre-armed, this will immediately call the respective
        // closure.
        let vec = vec![()];
        assert_eq!(manager.wait(Some(10)), Ok(vec.as_slice()));

        // As the closure will flip the atomic boolean we assert it has flipped correctly.
        assert!(COUNT.load(Ordering::SeqCst));

        // At this point we have called the closure, since the closure removes the event fd from the
        // interest list of the inner epoll, calling this again should timeout as there are no event
        // fd in the inner epolls interest list which could trigger.
        let vec = vec![];
        assert_eq!(manager.wait(Some(10)), Ok(vec.as_slice()));
        // As the `EventManager::wait` should timeout the value of the atomic boolean should not be
        // flipped.
        assert!(COUNT.load(Ordering::SeqCst));

        // SAFETY: `event_fd` is a valid file descriptor.
        unsafe { libc::close(event_fd) };
    }

    #[test]
    fn flip() {
        static COUNT: AtomicBool = AtomicBool::new(false);
        let mut manager = BufferedEventManager::default();
        // We set value to 1 so it will trigger on a read event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let fd = libc::eventfd(1, 0);
            assert_ne!(fd, -1);
            fd
        };
        manager
            .add(
                event_fd,
                EventSet::IN,
                Box::new(|_, _| {
                    // Flips the atomic.
                    let cur = COUNT.load(Ordering::SeqCst);
                    COUNT.store(!cur, Ordering::SeqCst);
                }),
            )
            .unwrap();

        // Assert the initial state of the atomic boolean.
        assert!(!COUNT.load(Ordering::SeqCst));

        // As the closure will flip the atomic boolean we assert it has flipped correctly.
        let vec = vec![()];
        assert_eq!(manager.wait(Some(10)), Ok(vec.as_slice()));
        // As the closure will flip the atomic boolean we assert it has flipped correctly.
        assert!(COUNT.load(Ordering::SeqCst));

        // The file descriptor has been pre-armed, this will immediately call the respective
        // closure.
        let vec = vec![()];
        assert_eq!(manager.wait(Some(10)), Ok(vec.as_slice()));
        // As the closure will flip the atomic boolean we assert it has flipped correctly.
        assert!(!COUNT.load(Ordering::SeqCst));
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn counters() {
        const SUBSCRIBERS: usize = 100;
        const FIRING: usize = 4;

        assert!(FIRING <= SUBSCRIBERS);

        let mut manager = BufferedEventManager::default();

        // Setup eventfd's and counters.
        let subscribers = (0..SUBSCRIBERS)
            .map(|_| {
                // SAFETY: Always safe.
                let event_fd = unsafe {
                    let raw_fd = libc::eventfd(0, 0);
                    assert_ne!(raw_fd, -1);
                    OwnedFd::from_raw_fd(raw_fd)
                };
                let counter = Arc::new(AtomicU64::new(0));
                let counter_clone = counter.clone();

                manager
                    .add(
                        event_fd.as_fd(),
                        EventSet::IN,
                        Box::new(move |_, _| counter_clone.fetch_add(1, Ordering::SeqCst)),
                    )
                    .unwrap();

                (event_fd, counter)
            })
            .collect::<Vec<_>>();

        // Arm random subscribers
        let mut rng = rand::thread_rng();
        let set = rand::seq::index::sample(&mut rng, SUBSCRIBERS, FIRING).into_vec();
        for i in &set {
            assert_ne!(
                // SAFETY: Always safe.
                unsafe {
                    libc::write(
                        subscribers[*i].0.as_raw_fd(),
                        &1u64 as *const u64 as *const libc::c_void,
                        std::mem::size_of::<u64>(),
                    )
                },
                -1
            );
        }

        // Check counter are the correct values
        let arr = [0; FIRING];
        assert_eq!(manager.wait(None), Ok(arr.as_slice()));
        for i in set {
            assert_eq!(subscribers[i].1.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn results() {
        let mut manager = BufferedEventManager::default();

        // We set value to 1 so it will trigger on a read event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let fd = libc::eventfd(1, 0);
            assert_ne!(fd, -1);
            fd
        };

        manager
            .add(event_fd, EventSet::IN, Box::new(|_, _| Ok(())))
            .unwrap();

        // We set value to 1 so it will trigger on a read event.
        // SAFETY: Always safe.
        let event_fd = unsafe {
            let fd = libc::eventfd(1, 0);
            assert_ne!(fd, -1);
            fd
        };

        manager
            .add(event_fd, EventSet::IN, Box::new(|_, _| Err(())))
            .unwrap();

        let arr = [Ok(()), Err(())];
        assert_eq!(manager.wait(None), Ok(arr.as_slice()));
    }
}
