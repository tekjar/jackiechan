use bytes::{Bytes, BytesMut};
use mqttbytes::v4::Publish;
use parking_lot::Mutex;
use std::mem;
use std::sync::Arc;

struct Buffer {
    buffer: BytesMut,
    outgoing: Vec<Option<Publish>>,
    last_pkid: u16,
}

impl Buffer {
    pub fn new(max_output_size: usize, max_packet_size: usize, max_inflight: usize) -> Buffer {
        Buffer {
            buffer: BytesMut::with_capacity(max_output_size + max_packet_size),
            outgoing: Vec::with_capacity(max_inflight),
            last_pkid: 0,
        }
    }

    pub fn take(&mut self, replace: Vec<Option<Publish>>) -> (Bytes, Vec<Option<Publish>>) {
        let buffer = self.buffer.split_off(0);
        let publishes = mem::replace(&mut self.outgoing, replace);
        (buffer.freeze(), publishes)
    }
}

pub struct Sender {
    buffer: Arc<Mutex<Buffer>>,
}
