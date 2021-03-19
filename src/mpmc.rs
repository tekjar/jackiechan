//! Note: This is currently a copy of stjepang's async-channel with minor
//! modifications. Use [async-channel](https://github.com/stjepang/async-channel)
//!
//! An async multi-producer multi-consumer channel.
//!
//! There are two kinds of channels:
//!
//! 1. [Bounded][`bounded()`] channel with limited capacity.
//!
//! A channel has the [`Sender`] and [`Receiver`] side. Both sides are cloneable and can be shared
//! among multiple threads.
//!
//! When all [`Sender`]s or all [`Receiver`]s are dropped, the channel becomes closed. When a
//! channel is closed, no more messages can be sent, but remaining messages can still be received.
//!
//! The channel can also be closed manually by calling [`Sender::close()`] or
//! [`Receiver::close()`].
//!
//! # Examples
//!
//! ```
//! let (s, r) = jackiechan::mpmc::bounded(1);
//!
//! assert_eq!(s.send("Hello"), Ok(()));
//! assert_eq!(r.recv(), Ok("Hello"));
//! ```

use std::error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::usize;

use concurrent_queue::{ConcurrentQueue, PopError, PushError};
use event_listener::{Event, EventListener};
use futures_core::stream::Stream;

struct Channel<T> {
    /// Inner message queue.
    queue: ConcurrentQueue<T>,

    /// Send operations waiting while the channel is full.
    send_ops: Event,

    /// Receive operations waiting while the channel is empty and not closed.
    recv_ops: Event,

    /// Stream operations while the channel is empty and not closed.
    stream_ops: Event,

    /// The number of currently active `Sender`s.
    sender_count: AtomicUsize,

    /// The number of currently active `Receivers`s.
    receiver_count: AtomicUsize,
}

impl<T> Channel<T> {
    /// Closes the channel and notifies all blocked operations.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    fn close(&self) -> bool {
        if self.queue.close() {
            // Notify all send operations.
            self.send_ops.notify(usize::MAX);

            // Notify all receive and stream operations.
            self.recv_ops.notify(usize::MAX);
            self.stream_ops.notify(usize::MAX);

            true
        } else {
            false
        }
    }
}

/// Creates a bounded channel.
///
/// The created channel has space to hold at most `cap` messages at a time.
///
/// # Panics
///
/// Capacity must be a positive number. If `cap` is zero, this function will panic.
///
/// # Examples
///
/// ```
/// use jackiechan::mpmc::{bounded, TryRecvError, TrySendError};
///
/// let (s, r) = bounded(1);
///
/// assert_eq!(s.send(10), Ok(()));
/// assert_eq!(s.try_send(20), Err(TrySendError::Full(20)));
///
/// assert_eq!(r.recv(), Ok(10));
/// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    assert!(cap > 0, "capacity cannot be zero");

    let channel = Arc::new(Channel {
        queue: ConcurrentQueue::bounded(cap),
        send_ops: Event::new(),
        recv_ops: Event::new(),
        stream_ops: Event::new(),
        sender_count: AtomicUsize::new(1),
        receiver_count: AtomicUsize::new(1),
    });

    let s = Sender {
        channel: channel.clone(),
    };
    let r = Receiver {
        channel,
        listener: None,
    };
    (s, r)
}

/// The sending side of a channel.
///
/// Senders can be cloned and shared among threads. When all senders associated with a channel are
/// dropped, the channel becomes closed.
///
/// The channel can also be closed manually by calling [`Sender::close()`].
pub struct Sender<T> {
    /// Inner channel state.
    channel: Arc<Channel<T>>,
}

impl<T> Sender<T> {
    /// Attempts to send a message into the channel.
    ///
    /// If the channel is full or closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, TrySendError};
    ///
    /// let (s, r) = bounded(1);
    ///
    /// assert_eq!(s.try_send(1), Ok(()));
    /// assert_eq!(s.try_send(2), Err(TrySendError::Full(2)));
    ///
    /// drop(r);
    /// assert_eq!(s.try_send(3), Err(TrySendError::Closed(3)));
    /// ```
    pub fn try_send(&self, msg: T) -> Result<(), TrySendError<T>> {
        match self.channel.queue.push(msg) {
            Ok(()) => {
                // Notify a single blocked receive operation. If the notified operation then
                // receives a message or gets canceled, it will notify another blocked receive
                // operation.
                self.channel.recv_ops.notify(1);

                // Notify all blocked streams.
                self.channel.stream_ops.notify(usize::MAX);

                Ok(())
            }
            Err(PushError::Full(msg)) => Err(TrySendError::Full(msg)),
            Err(PushError::Closed(msg)) => Err(TrySendError::Closed(msg)),
        }
    }

    /// Sends a message into the channel.
    ///
    /// If the channel is full, this method waits until there is space for a message.
    ///
    /// If the channel is closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, SendError};
    ///
    /// let (s, r) = bounded(100);
    ///
    /// assert_eq!(s.send(1), Ok(()));
    /// drop(r);
    /// assert_eq!(s.send(2), Err(SendError(2)));
    /// ```
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        let mut listener = None;
        let mut msg = msg;

        loop {
            // Attempt to send a message.
            match self.try_send(msg) {
                Ok(()) => {
                    // If the capacity is larger than 1, notify another blocked send operation.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.send_ops.notify(1),
                    }
                    return Ok(());
                }
                Err(TrySendError::Closed(msg)) => return Err(SendError(msg)),
                Err(TrySendError::Full(m)) => msg = m,
            }

            // Sending failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.send_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    l.wait();
                }
            }
        }
    }

    /// Sends a message into the channel.
    ///
    /// If the channel is full, this method waits until there is space for a message.
    ///
    /// If the channel is closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use jackiechan::mpmc::{bounded, SendError};
    ///
    /// let (s, r) = bounded(100);
    ///
    /// assert_eq!(s.async_send(1).await, Ok(()));
    /// drop(r);
    /// assert_eq!(s.async_send(2).await, Err(SendError(2)));
    /// # });
    /// ```
    pub async fn async_send(&self, msg: T) -> Result<(), SendError<T>> {
        let mut listener = None;
        let mut msg = msg;

        loop {
            // Attempt to send a message.
            match self.try_send(msg) {
                Ok(()) => {
                    // If the capacity is larger than 1, notify another blocked send operation.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.send_ops.notify(1),
                    }
                    return Ok(());
                }
                Err(TrySendError::Closed(msg)) => return Err(SendError(msg)),
                Err(TrySendError::Full(m)) => msg = m,
            }

            // Sending failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.send_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    l.await;
                }
            }
        }
    }

    /// Closes the channel.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    ///
    /// The remaining messages can still be received.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvError};
    ///
    /// let (s, r) = bounded(10);
    /// assert_eq!(s.send(1), Ok(()));
    /// assert!(s.close());
    ///
    /// assert_eq!(r.recv(), Ok(1));
    /// assert_eq!(r.recv(), Err(RecvError));
    /// ```
    pub fn close(&self) -> bool {
        self.channel.close()
    }

    /// Returns `true` if the channel is closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvError};
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert!(!s.is_closed());
    ///
    /// s.close();
    /// assert!(s.is_closed());
    /// ```
    pub fn is_closed(&self) -> bool {
        self.channel.queue.is_closed()
    }

    /// Returns `true` if the channel is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(10);
    ///
    /// assert!(s.is_empty());
    /// s.send(1);
    /// assert!(!s.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.channel.queue.is_empty()
    }

    /// Returns `true` if the channel is full.
    ///
    /// Unbounded channels are never full.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(1);
    ///
    /// assert!(!s.is_full());
    /// s.send(1);
    /// assert!(s.is_full());
    /// ```
    pub fn is_full(&self) -> bool {
        self.channel.queue.is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(10);
    /// assert_eq!(s.len(), 0);
    ///
    /// s.send(1);
    /// s.send(2);
    /// assert_eq!(s.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.channel.queue.len()
    }

    /// Returns the channel capacity if it's bounded.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded};
    ///
    /// let (s, r) = bounded::<i32>(5);
    /// assert_eq!(s.capacity(), Some(5));
    /// ```
    pub fn capacity(&self) -> Option<usize> {
        self.channel.queue.capacity()
    }

    /// Returns the number of receivers for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert_eq!(s.receiver_count(), 1);
    ///
    /// let r2 = r.clone();
    /// assert_eq!(s.receiver_count(), 2);
    /// ```
    pub fn receiver_count(&self) -> usize {
        self.channel.receiver_count.load(Ordering::SeqCst)
    }

    /// Returns the number of senders for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert_eq!(s.sender_count(), 1);
    ///
    /// let s2 = s.clone();
    /// assert_eq!(s.sender_count(), 2);
    /// ```
    pub fn sender_count(&self) -> usize {
        self.channel.sender_count.load(Ordering::SeqCst)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Decrement the sender count and close the channel if it drops down to zero.
        if self.channel.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.channel.close();
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sender {{ .. }}")
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Sender<T> {
        let count = self.channel.sender_count.fetch_add(1, Ordering::Relaxed);

        // Make sure the count never overflows, even if lots of sender clones are leaked.
        if count > usize::MAX / 2 {
            process::abort();
        }

        Sender {
            channel: self.channel.clone(),
        }
    }
}

/// The receiving side of a channel.
///
/// Receivers can be cloned and shared among threads. When all receivers associated with a channel
/// are dropped, the channel becomes closed.
///
/// The channel can also be closed manually by calling [`Receiver::close()`].
///
/// Receivers implement the [`Stream`] trait.
pub struct Receiver<T> {
    /// Inner channel state.
    channel: Arc<Channel<T>>,

    /// Listens for a send or close event to unblock this stream.
    listener: Option<EventListener>,
}

impl<T> Receiver<T> {
    /// Attempts to receive a message from the channel.
    ///
    /// If the channel is empty or closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, TryRecvError};
    ///
    /// let (s, r) = bounded(10);
    /// assert_eq!(s.send(1), Ok(()));
    ///
    /// assert_eq!(r.try_recv(), Ok(1));
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    ///
    /// drop(s);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Closed));
    /// ```
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.channel.queue.pop() {
            Ok(msg) => {
                // Notify a single blocked send operation. If the notified operation then sends a
                // message or gets canceled, it will notify another blocked send operation.
                self.channel.send_ops.notify(1);

                Ok(msg)
            }
            Err(PopError::Empty) => Err(TryRecvError::Empty),
            Err(PopError::Closed) => Err(TryRecvError::Closed),
        }
    }

    /// Receives a message from the channel.
    ///
    /// If the channel is empty, this method waits until there is a message.
    ///
    /// If the channel is closed, this method receives a message or returns an error if there are
    /// no more messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvError};
    ///
    /// let (s, r) = bounded(10);
    ///
    /// assert_eq!(s.send(1), Ok(()));
    /// drop(s);
    ///
    /// assert_eq!(r.recv(), Ok(1));
    /// assert_eq!(r.recv(), Err(RecvError));
    /// ```
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut listener = None;

        loop {
            // Attempt to receive a message.
            match self.try_recv() {
                Ok(msg) => {
                    // If the capacity is larger than 1, notify another blocked receive operation.
                    // There is no need to notify stream operations because all of them get
                    // notified every time a message is sent into the channel.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.recv_ops.notify(1),
                    }
                    return Ok(msg);
                }
                Err(TryRecvError::Closed) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }

            // Receiving failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.recv_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    l.wait();
                }
            }
        }
    }

    /// Receives a message from the channel.
    ///
    /// If the channel is empty, this method waits until there is a message.
    ///
    /// If the channel is closed, this method receives a message or returns an error if there are
    /// no more messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvTimeoutError};
    /// use std::time::Duration;
    ///
    /// let (s, r) = bounded(10);
    ///
    /// assert_eq!(s.send(1), Ok(()));
    /// assert_eq!(r.recv_timeout(Duration::from_secs(1)), Ok(1));
    /// assert_eq!(r.recv_timeout(Duration::from_secs(1)), Err(RecvTimeoutError::Timeout));
    /// ```
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let mut listener = None;

        loop {
            // Attempt to receive a message.
            match self.try_recv() {
                Ok(msg) => {
                    // If the capacity is larger than 1, notify another blocked receive operation.
                    // There is no need to notify stream operations because all of them get
                    // notified every time a message is sent into the channel.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.recv_ops.notify(1),
                    }
                    return Ok(msg);
                }
                Err(TryRecvError::Closed) => return Err(RecvTimeoutError::Closed),
                Err(TryRecvError::Empty) => {}
            }

            // Receiving failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.recv_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    if !l.wait_timeout(timeout) {
                        return Err(RecvTimeoutError::Timeout);
                    }
                }
            }
        }
    }

    /// Receives a message from the channel.
    ///
    /// If the channel is empty, this method waits until there is a message.
    ///
    /// If the channel is closed, this method receives a message or returns an error if there are
    /// no more messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvTimeoutError};
    /// use std::time::{Instant, Duration};
    ///
    /// let (s, r) = bounded(10);
    ///
    /// assert_eq!(s.send(1), Ok(()));
    ///
    /// assert_eq!(r.recv_deadline(Instant::now() + Duration::from_secs(1)), Ok(1));
    /// assert_eq!(r.recv_deadline(Instant::now() + Duration::from_secs(1)), Err(RecvTimeoutError::Timeout));
    /// ```
    pub fn recv_deadline(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        let mut listener = None;

        loop {
            // Attempt to receive a message.
            match self.try_recv() {
                Ok(msg) => {
                    // If the capacity is larger than 1, notify another blocked receive operation.
                    // There is no need to notify stream operations because all of them get
                    // notified every time a message is sent into the channel.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.recv_ops.notify(1),
                    }
                    return Ok(msg);
                }
                Err(TryRecvError::Closed) => return Err(RecvTimeoutError::Closed),
                Err(TryRecvError::Empty) => {}
            }

            // Receiving failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.recv_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    if !l.wait_deadline(deadline) {
                        return Err(RecvTimeoutError::Timeout);
                    }
                }
            }
        }
    }

    /// Receives a message from the channel.
    /// If the channel is empty, this method waits until there is a message.
    ///
    /// If the channel is closed, this method receives a message or returns an error if there are
    /// no more messages.
    pub async fn async_recv(&self) -> Result<T, RecvError> {
        let mut listener = None;

        loop {
            // Attempt to receive a message.
            match self.try_recv() {
                Ok(msg) => {
                    // If the capacity is larger than 1, notify another blocked receive operation.
                    // There is no need to notify stream operations because all of them get
                    // notified every time a message is sent into the channel.
                    match self.channel.queue.capacity() {
                        Some(1) => {}
                        Some(_) | None => self.channel.recv_ops.notify(1),
                    }
                    return Ok(msg);
                }
                Err(TryRecvError::Closed) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }

            // Receiving failed - now start listening for notifications or wait for one.
            match listener.take() {
                None => {
                    // Start listening and then try receiving again.
                    listener = Some(self.channel.recv_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    l.await;
                }
            }
        }
    }

    /// Closes the channel.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    ///
    /// The remaining messages can still be received.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvError};
    ///
    /// let (s, r) = bounded(10);
    /// assert_eq!(s.send(1), Ok(()));
    ///
    /// assert!(r.close());
    /// assert_eq!(r.recv(), Ok(1));
    /// assert_eq!(r.recv(), Err(RecvError));
    /// ```
    pub fn close(&self) -> bool {
        self.channel.close()
    }

    /// Returns `true` if the channel is closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded, RecvError};
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert!(!r.is_closed());
    ///
    /// drop(s);
    /// assert!(r.is_closed());
    /// ```
    pub fn is_closed(&self) -> bool {
        self.channel.queue.is_closed()
    }

    /// Returns `true` if the channel is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(10);
    ///
    /// assert!(s.is_empty());
    /// s.send(1);
    /// assert!(!s.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.channel.queue.is_empty()
    }

    /// Returns `true` if the channel is full.
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(1);
    ///
    /// assert!(!r.is_full());
    /// s.send(1);
    /// assert!(r.is_full());
    /// ```
    pub fn is_full(&self) -> bool {
        self.channel.queue.is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded(10);
    /// assert_eq!(r.len(), 0);
    ///
    /// s.send(1);
    /// s.send(2);
    /// assert_eq!(r.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.channel.queue.len()
    }

    /// Returns the channel capacity if it's bounded.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::{bounded};
    ///
    /// let (s, r) = bounded::<i32>(5);
    /// assert_eq!(r.capacity(), Some(5));
    /// ```
    pub fn capacity(&self) -> Option<usize> {
        self.channel.queue.capacity()
    }

    /// Returns the number of receivers for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert_eq!(r.receiver_count(), 1);
    ///
    /// let r2 = r.clone();
    /// assert_eq!(r.receiver_count(), 2);
    /// ```
    pub fn receiver_count(&self) -> usize {
        self.channel.receiver_count.load(Ordering::SeqCst)
    }

    /// Returns the number of senders for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use jackiechan::mpmc::bounded;
    ///
    /// let (s, r) = bounded::<()>(10);
    /// assert_eq!(r.sender_count(), 1);
    ///
    /// let s2 = s.clone();
    /// assert_eq!(r.sender_count(), 2);
    /// ```
    pub fn sender_count(&self) -> usize {
        self.channel.sender_count.load(Ordering::SeqCst)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Decrement the receiver count and close the channel if it drops down to zero.
        if self.channel.receiver_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.channel.close();
        }
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Receiver {{ .. }}")
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Receiver<T> {
        let count = self.channel.receiver_count.fetch_add(1, Ordering::Relaxed);

        // Make sure the count never overflows, even if lots of receiver clones are leaked.
        if count > usize::MAX / 2 {
            process::abort();
        }

        Receiver {
            channel: self.channel.clone(),
            listener: None,
        }
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // If this stream is listening for events, first wait for a notification.
            if let Some(listener) = self.listener.as_mut() {
                futures_core::ready!(Pin::new(listener).poll(cx));
                self.listener = None;
            }

            loop {
                // Attempt to receive a message.
                match self.try_recv() {
                    Ok(msg) => {
                        // The stream is not blocked on an event - drop the listener.
                        self.listener = None;
                        return Poll::Ready(Some(msg));
                    }
                    Err(TryRecvError::Closed) => {
                        // The stream is not blocked on an event - drop the listener.
                        self.listener = None;
                        return Poll::Ready(None);
                    }
                    Err(TryRecvError::Empty) => {}
                }

                // Receiving failed - now start listening for notifications or wait for one.
                match self.listener.as_mut() {
                    None => {
                        // Create a listener and try sending the message again.
                        self.listener = Some(self.channel.stream_ops.listen());
                    }
                    Some(_) => {
                        // Go back to the outer loop to poll the listener.
                        break;
                    }
                }
            }
        }
    }
}

/// An error returned from [`Sender::send()`].
///
/// Received because the channel is closed.
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct SendError<T>(pub T);

impl<T> SendError<T> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> error::Error for SendError<T> {}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError(..)")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sending into a closed channel")
    }
}

/// An error returned from [`Sender::try_send()`].
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum TrySendError<T> {
    /// The channel is full but not closed.
    Full(T),

    /// The channel is closed.
    Closed(T),
}

impl<T> TrySendError<T> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        match self {
            TrySendError::Full(t) => t,
            TrySendError::Closed(t) => t,
        }
    }

    /// Returns `true` if the channel is full but not closed.
    pub fn is_full(&self) -> bool {
        match self {
            TrySendError::Full(_) => true,
            TrySendError::Closed(_) => false,
        }
    }

    /// Returns `true` if the channel is closed.
    pub fn is_closed(&self) -> bool {
        match self {
            TrySendError::Full(_) => false,
            TrySendError::Closed(_) => true,
        }
    }
}

impl<T> error::Error for TrySendError<T> {}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TrySendError::Full(..) => write!(f, "Full(..)"),
            TrySendError::Closed(..) => write!(f, "Closed(..)"),
        }
    }
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TrySendError::Full(..) => write!(f, "sending into a full channel"),
            TrySendError::Closed(..) => write!(f, "sending into a closed channel"),
        }
    }
}

/// An error returned from [`Receiver::recv()`].
///
/// Received because the channel is empty and closed.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct RecvError;

impl error::Error for RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiving from an empty and closed channel")
    }
}

/// An error returned from [`Receiver::try_recv()`].
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum TryRecvError {
    /// The channel is empty but not closed.
    Empty,

    /// The channel is empty and closed.
    Closed,
}

impl TryRecvError {
    /// Returns `true` if the channel is empty but not closed.
    pub fn is_empty(&self) -> bool {
        match self {
            TryRecvError::Empty => true,
            TryRecvError::Closed => false,
        }
    }

    /// Returns `true` if the channel is empty and closed.
    pub fn is_closed(&self) -> bool {
        match self {
            TryRecvError::Empty => false,
            TryRecvError::Closed => true,
        }
    }
}

impl error::Error for TryRecvError {}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TryRecvError::Empty => write!(f, "receiving from an empty channel"),
            TryRecvError::Closed => write!(f, "receiving from an empty and closed channel"),
        }
    }
}

/// An error returned from the [`recv_timeout`] method.
///
/// [`recv_timeout`]: super::Receiver::recv_timeout
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RecvTimeoutError {
    /// A message could not be received because the channel is empty and the operation timed out.
    ///
    /// If this is a zero-capacity channel, then the error indicates that there was no sender
    /// available to send a message and the operation timed out.
    Timeout,

    /// The message could not be received because the channel is empty and disconnected.
    Closed,
}

impl error::Error for RecvTimeoutError {}

impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RecvTimeoutError::Timeout => "timed out waiting on receive operation".fmt(f),
            RecvTimeoutError::Closed => "channel is empty and disconnected".fmt(f),
        }
    }
}
