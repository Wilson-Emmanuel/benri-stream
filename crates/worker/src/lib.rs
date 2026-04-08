//! Library surface for the worker crate. Exposes the handler types
//! (and the dispatch trait they plug into) so integration tests can
//! construct a `HandlerDispatch` from real use-case implementations and
//! drive a task end-to-end without booting the consumer loop.
//!
//! Everything else (consumer, poller, recovery loop, system checker)
//! stays binary-local in `main.rs`.

pub mod handlers;
