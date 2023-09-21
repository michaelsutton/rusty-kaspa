use super::MiningCounters;
use kaspa_core::{
    info,
    task::{
        service::{AsyncService, AsyncServiceFuture},
        tick::{TickReason, TickService},
    },
    trace,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const MONITOR: &str = "mempool-monitor";

pub struct MiningMonitor {
    // Counters
    counters: Arc<MiningCounters>,

    // Tick service
    tick_service: Arc<TickService>,
}

impl MiningMonitor {
    pub fn new(counters: Arc<MiningCounters>, tick_service: Arc<TickService>) -> MiningMonitor {
        MiningMonitor { counters, tick_service }
    }

    pub async fn worker(self: &Arc<MiningMonitor>) {
        let mut last_snapshot = self.counters.snapshot();
        let mut last_log_time = Instant::now();
        let snapshot_interval = 10;
        loop {
            if let TickReason::Shutdown = self.tick_service.tick(Duration::from_secs(snapshot_interval)).await {
                // Let the system print final logs before exiting
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }

            let snapshot = self.counters.snapshot();
            if snapshot == last_snapshot {
                // No update, avoid printing useless info
                last_log_time = Instant::now();
                continue;
            }

            // Subtract the snapshots
            let delta = &snapshot - &last_snapshot;
            let now = Instant::now();
            let elapsed = (now - last_log_time).as_secs_f64();

            info!("Processed {} unique transactions in the last {:.2}s ({:.2} avg txs/s, in: {} via RPC, {} via P2P, out: {} via accepted blocks, {:.2}% collisions)",
                delta.tx_accepted_counts,
                elapsed,
                delta.tx_accepted_counts as f64 / elapsed,
                delta.high_priority_tx_counts,
                delta.low_priority_tx_counts,
                delta.block_tx_counts,
                delta.collision_ratio() * 100.0,
            );

            last_snapshot = snapshot;
            last_log_time = now;
        }

        trace!("mempool monitor thread exiting");
    }
}

// service trait implementation for Monitor
impl AsyncService for MiningMonitor {
    fn ident(self: Arc<Self>) -> &'static str {
        MONITOR
    }

    fn start(self: Arc<Self>) -> AsyncServiceFuture {
        Box::pin(async move {
            self.worker().await;
            Ok(())
        })
    }

    fn signal_exit(self: Arc<Self>) {
        trace!("sending an exit signal to {}", MONITOR);
    }

    fn stop(self: Arc<Self>) -> AsyncServiceFuture {
        Box::pin(async move {
            trace!("{} stopped", MONITOR);
            Ok(())
        })
    }
}