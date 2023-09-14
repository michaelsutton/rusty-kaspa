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
#[allow(clippy::type_complexity)]
pub static MEMPOOL_SIZE_LOG: Lazy<Mutex<Vec<(u64, u64, u64, f64)>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(10_000_000)));

pub fn log_submitted_txs_count(count: u64) {
    SUBMIT_TXS_LOG.lock().push((unix_now(), count))
}

pub fn log_mempool_size(mempool_size: u64, submitted_txs: u64) {
    let mut v = MEMPOOL_SIZE_LOG.lock();
    let now = unix_now();
    match v.iter().rev().find(|e| now > e.0 + 5000) {
        Some(prev) => {
            let time_delta = now as i64 - prev.0 as i64;
            let prev_mempool = prev.1 as i64;
            let current_mempool = mempool_size as i64;
            let submit_delta = submitted_txs as i64 - prev.2 as i64;
            let rate = (prev_mempool - (current_mempool - submit_delta)) as f64 / (time_delta as f64 / 1000.0);
            v.push((now, mempool_size, submitted_txs, rate.clamp(0.0, 100_000.0)));
        }
        None => v.push((now, mempool_size, submitted_txs, 0.0)),
    }
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
