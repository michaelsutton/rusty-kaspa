use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Returns the number of milliseconds since UNIX EPOCH
#[inline]
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

pub static SB_TIMING_LOG: Lazy<Mutex<Vec<(u64, u64)>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(10_000_000)));
pub static BBT_TIMING_LOG: Lazy<Mutex<Vec<(u64, u64)>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(10_000_000)));
pub static SUBMIT_TXS_LOG: Lazy<Mutex<Vec<(u64, u64)>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(10_000_000)));
pub static MEMPOOL_SIZE_LOG: Lazy<Mutex<Vec<(u64, u64)>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(10_000_000)));

pub fn log_submitted_txs_count(count: u64) {
    SUBMIT_TXS_LOG.lock().push((unix_now(), count))
}

pub fn log_mempool_size(size: u64) {
    MEMPOOL_SIZE_LOG.lock().push((unix_now(), size))
}

/// Stopwatch which reports on drop if the timed operation passed the threshold `TR` in milliseconds
pub struct Stopwatch<const TR: u64 = 1000> {
    name: &'static str,
    start: Instant,
}

impl Stopwatch {
    pub fn new(name: &'static str) -> Self {
        Self { name, start: Instant::now() }
    }
}

impl<const TR: u64> Stopwatch<TR> {
    pub fn with_threshold(name: &'static str) -> Self {
        Self { name, start: Instant::now() }
    }
}

impl<const TR: u64> Drop for Stopwatch<TR> {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        match self.name {
            "bbt" => BBT_TIMING_LOG.lock().push((unix_now(), elapsed.as_millis() as u64)),
            "sb" => SB_TIMING_LOG.lock().push((unix_now(), elapsed.as_millis() as u64)),
            _ => {}
        }
        if elapsed > Duration::from_millis(TR) {
            kaspa_core::trace!("[{}] Abnormal time: {:#?}", self.name, elapsed);
        }
    }
}
