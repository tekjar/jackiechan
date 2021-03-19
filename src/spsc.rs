use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use event_listener::{Event, EventListener};


pub fn bounded<T>(size: usize) -> (Sender<T>, Receiver<T>) {
    let send_buffer = VecDeque::with_capacity(size);
    let send_buffer = Arc::new(Mutex::new(send_buffer));

    let event = Event::new();
    let listener = event.listen();

    let sender = Sender::new( send_buffer.clone(), event);
    let receiver = Receiver::new(size, send_buffer, listener);
    (sender, receiver)
}

pub struct Receiver<T> {
    send_buffer: Arc<Mutex<VecDeque<T>>>,
    recv_buffer: VecDeque<T>,
    listener: EventListener
}

impl<T> Receiver<T> {
    fn new(size: usize, send_buffer: Arc<Mutex<VecDeque<T>>>, listener: EventListener) -> Receiver<T> {
        Receiver {
            send_buffer,
            recv_buffer: VecDeque::with_capacity(size),
            listener
        }
    }
}

pub struct Sender<T> {
    send_buffer: Arc<Mutex<VecDeque<T>>>,
    event: Event
}

impl<T> Sender<T> {
    fn new(send_buffer: Arc<Mutex<VecDeque<T>>>, event: Event) -> Sender<T> {
        Sender {
            send_buffer,
            event
        }
    }

    // fn send()
}
