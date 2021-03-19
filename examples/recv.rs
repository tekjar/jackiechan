use jackiechan::mpmc::{bounded};
use std::time::{Duration, Instant};
use jackiechan::RecvTimeoutError;

fn main() {
    let (s, r) = bounded(10);
    assert_eq!(s.send(1), Ok(()));

    assert_eq!(r.recv_timeout(Duration::from_secs(1)), Ok(1));
    assert_eq!(
        r.recv_timeout(Duration::from_secs(1)),
        Err(RecvTimeoutError::Timeout)
    );

    assert_eq!(s.send(1), Ok(()));
    assert_eq!(
        r.recv_deadline(Instant::now() + Duration::from_secs(1)),
        Ok(1)
    );
    assert_eq!(
        r.recv_deadline(Instant::now() + Duration::from_secs(1)),
        Err(RecvTimeoutError::Timeout)
    );
}
