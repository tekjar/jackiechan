use crate::{RecvError, SendError, TrySendError};
use event_listener::Event;
use std::collections::VecDeque;
use std::mem;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn bounded<T>(size: usize) -> (Sender<T>, Receiver<T>) {
    let send_buffer = VecDeque::with_capacity(size);
    let send_buffer = Arc::new(Mutex::new(send_buffer));

    let sender = Arc::new(Event::new());
    let receiver = Arc::new(Event::new());

    let s = Sender::new(size, send_buffer.clone(), receiver.clone(), sender.clone());

    let r = Receiver::new(size, send_buffer, sender, receiver);
    (s, r)
}

pub struct Receiver<T> {
    send_buffer: Arc<Mutex<VecDeque<T>>>,
    recv_buffer: VecDeque<T>,
    sender_notify: Arc<Event>,
    sender_listen: Arc<Event>,
}

impl<T> Receiver<T> {
    fn new(
        size: usize,
        send_buffer: Arc<Mutex<VecDeque<T>>>,
        sender_notify: Arc<Event>,
        sender_listen: Arc<Event>,
    ) -> Receiver<T> {
        Receiver {
            send_buffer,
            recv_buffer: VecDeque::with_capacity(size),
            sender_notify,
            sender_listen,
        }
    }

    pub fn recv(&mut self) -> Result<T, RecvError> {
        // Swap send buffer and recv buffer if recv buffer is full
        if self.recv_buffer.is_empty() {
            let mut send_buffer = self.send_buffer.lock().unwrap();
            mem::swap(&mut *send_buffer, &mut self.recv_buffer);
        }

        // If recv buffer is still empty, wait for a send event
        if self.recv_buffer.is_empty() {
            self.sender_notify.listen().wait();
        }

        let v = self.recv_buffer.pop_front().unwrap();
        Ok(v)
    }
}

pub struct Sender<T> {
    max: usize,
    buffer: Arc<Mutex<VecDeque<T>>>,
    receiver_notify: Arc<Event>,
    receiver_listen: Arc<Event>,
}

impl<T> Sender<T> {
    fn new(
        max: usize,
        send_buffer: Arc<Mutex<VecDeque<T>>>,
        receiver_notify: Arc<Event>,
        receiver_listen: Arc<Event>,
    ) -> Sender<T> {
        Sender {
            max,
            buffer: send_buffer,
            receiver_notify,
            receiver_listen,
        }
    }

    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.len() >= self.max {
            self.receiver_notify.notify(1);
            return Err(TrySendError::Full(message));
        }

        buffer.push_back(message);
        Ok(())
    }

    pub fn send(&self, message: T) -> Result<(), SendError<T>> {
        let message = match self.try_send(message) {
            Ok(v) => return Ok(v),
            Err(TrySendError::Full(v)) => v,
            Err(TrySendError::Closed(v)) => return Err(SendError(v)),
        };

        self.receiver_listen.listen().wait();
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_back(message);
        Ok(())
    }

    pub fn send_timeout(&self, message: T, timeout: Duration) -> Result<(), TrySendError<T>> {
        let message = match self.try_send(message) {
            Ok(v) => return Ok(v),
            Err(TrySendError::Full(v)) => v,
            Err(TrySendError::Closed(v)) => return Err(TrySendError::Closed(v)),
        };

        self.receiver_listen.listen().wait_timeout(timeout);
        self.try_send(message)
    }
}

#[cfg(test)]
mod test {
    use crate::spsc::bounded;
    use crate::TrySendError;

    #[test]
    fn send_and_recv_works() {
        let (tx, mut rx) = bounded(5);
        for i in 0..5 {
            tx.send(i).unwrap()
        }

        for i in 0..5 {
            assert_eq!(i, rx.recv().unwrap())
        }
    }

    #[test]
    fn try_send_detects_blocks() {
        let (tx, mut rx) = bounded(5);
        for i in 0..5 {
            tx.try_send(i).unwrap()
        }

        assert_eq!(tx.try_send(6), Err(TrySendError::Full(6)));
    }
}
