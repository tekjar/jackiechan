//! Channel like interface on shared state
#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod channel;

/// Channel
pub use channel::{Sender, Receiver, SendError, TrySendError, bounded};