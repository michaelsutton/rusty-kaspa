use super::MiningCounters;
use crate::manager::MiningManagerProxy;
use kaspa_consensusmanager::ConsensusManager;
use kaspa_core::{
    debug, info,
    task::{
        service::{AsyncService, AsyncServiceFuture},
        tick::{TickReason, TickService},
    },
    trace,
};
use kaspa_txscript::caches::TxScriptCacheCounters;
use std::{sync::Arc, time::Duration};

const MONITOR: &str = "mempool-monitor";

pub struct MiningMonitor {
    consensus_manager: Arc<ConsensusManager>,

    mining_manager: MiningManagerProxy,

    // Counters
    counters: Arc<MiningCounters>,

    tx_script_cache_counters: Arc<TxScriptCacheCounters>,

    // Tick service
    tick_service: Arc<TickService>,
}

impl MiningMonitor {
    pub fn new(
        consensus_manager: Arc<ConsensusManager>,
        mining_manager: MiningManagerProxy,
        counters: Arc<MiningCounters>,
        tx_script_cache_counters: Arc<TxScriptCacheCounters>,
        tick_service: Arc<TickService>,
    ) -> MiningMonitor {
        MiningMonitor { consensus_manager, mining_manager, counters, tx_script_cache_counters, tick_service }
    }

    pub async fn worker(self: &Arc<MiningMonitor>) {
        let mut last_snapshot = self.counters.snapshot();
        let mut last_tx_script_cache_snapshot = self.tx_script_cache_counters.snapshot();
        let snapshot_interval = 1;
        let mut i = 0u64;
        loop {
            if let TickReason::Shutdown = self.tick_service.tick(Duration::from_secs(snapshot_interval)).await {
                // Let the system print final logs before exiting
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }

            let consensus = self.consensus_manager.consensus().unguarded_session();
            let response = self
                .mining_manager
                .clone()
                .get_realtime_feerate_estimations_verbose(&consensus, kaspa_addresses::Prefix::Mainnet)
                .await
                .unwrap();
            debug!(
                "mempool_stats{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}",
                response.estimations.priority_bucket.feerate,
                response.estimations.priority_bucket.estimated_seconds,
                response.estimations.normal_buckets[0].feerate,
                response.estimations.normal_buckets[0].estimated_seconds,
                response.estimations.normal_buckets[1].feerate,
                response.estimations.normal_buckets[1].estimated_seconds,
                response.estimations.low_buckets[0].feerate,
                response.estimations.low_buckets[0].estimated_seconds,
                response.mempool_ready_transactions_count,
                response.mempool_ready_transactions_total_mass,
                response.next_block_template_feerate_min,
                response.next_block_template_feerate_median,
                response.next_block_template_feerate_max,
            );

            i = i.overflowing_add(1).0;
            if i % 10 != 0 {
                continue;
            }

            let snapshot = self.counters.snapshot();
            let tx_script_cache_snapshot = self.tx_script_cache_counters.snapshot();
            if snapshot == last_snapshot {
                // No update, avoid printing useless info
                continue;
            }

            // Subtract the snapshots
            let delta = &snapshot - &last_snapshot;
            let tx_script_cache_delta = &tx_script_cache_snapshot - &last_tx_script_cache_snapshot;

            if delta.has_tps_activity() {
                info!(
                    "Tx throughput stats: {:.2} u-tps, {:.2}% e-tps (in: {} via RPC, {} via P2P, out: {} via accepted blocks)",
                    delta.u_tps(),
                    delta.e_tps() * 100.0,
                    delta.high_priority_tx_counts,
                    delta.low_priority_tx_counts,
                    delta.tx_accepted_counts,
                );
                debug!("Realtime feerate estimations: {}", response.estimations);
            }
            if delta.tx_evicted_counts > 0 {
                info!(
                    "Mempool stats: {} transactions were evicted from the mempool in favor of incoming higher feerate transactions",
                    delta.tx_evicted_counts
                );
            }
            if tx_script_cache_snapshot != last_tx_script_cache_snapshot {
                debug!(
                    "UTXO set stats: {} spent, {} created ({} signatures validated, {} cache hits, {:.2} hit ratio)",
                    delta.input_counts,
                    delta.output_counts,
                    tx_script_cache_delta.insert_counts,
                    tx_script_cache_delta.get_counts,
                    tx_script_cache_delta.hit_ratio()
                );
            }
            if delta.txs_sample + delta.orphans_sample > 0 {
                debug!(
                    "Mempool sample: {} ready out of {} txs, {} orphans, {} cached as accepted",
                    delta.ready_txs_sample, delta.txs_sample, delta.orphans_sample, delta.accepted_sample
                );
            }

            last_snapshot = snapshot;
            last_tx_script_cache_snapshot = tx_script_cache_snapshot;
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
