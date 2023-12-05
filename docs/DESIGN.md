# Event Manager Design


`EventManager` is a wrapper over [epoll](https://man7.org/linux/man-pages/man7/epoll.7.html) that
allows for more ergonomic usage with many events.

## Interest List Updates

Event actions are represented by a closure of the type `Box<dyn Fn(&mut EventManager, EventSet)>`.

The mutable reference to the `EventManager` can be used to:

- Add a new event with `EventManager::add`.
- Remove an existing event with `EventManager::del`.
- Wait on further events with `EventManager::wait`.

The `EventSet` communicates the specific event/s that fired.