//! Channel like interface on shared state
#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod channel;

/// Channel
pub use channel::{
    bounded, Receiver, RecvError, RecvTimeoutError, SendError, Sender, TryRecvError, TrySendError,
};
