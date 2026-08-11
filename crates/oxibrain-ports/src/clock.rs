//! Clock access goes through ClockPort so tests control time.

use crate::time::Timestamp;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait ClockPort: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemClock;

impl ClockPort for SystemClock {
    fn now(&self) -> Timestamp {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock pre-epoch");
        Timestamp(dur.as_millis() as i64)
    }
}

#[derive(Debug)]
pub struct FakeClock {
    current: std::sync::atomic::AtomicI64,
}

impl FakeClock {
    pub fn new(start: Timestamp) -> Self {
        Self {
            current: start.0.into(),
        }
    }
    pub fn advance(&self, by_millis: i64) {
        self.current
            .fetch_add(by_millis, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ClockPort for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.current.load(std::sync::atomic::Ordering::Relaxed))
    }
}
