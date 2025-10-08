/* The finality probability for a given depth D is calculated as
 * (Number of blocks reorged after reaching depth D) / (Total number of blocks that reached depth D)
 */

use async_trait::async_trait;
use clap::Parser;
use futures::FutureExt;
use kaspa_consensus::{
    consensus::storage::ConsensusStorage,
    model::stores::{ghostdag::GhostdagStoreReader, virtual_state::VirtualStateStoreReader},
};
use kaspa_consensus_core::{
    config::ConfigBuilder,
    network::{NetworkId, NetworkType},
    Hash,
};
use kaspa_database::prelude::{ConnBuilder, StoreResultExtensions, DB};
use log::warn;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{select, sync::Mutex as TokioMutex, time::sleep};
use workflow_core::channel::DuplexChannel;
use workflow_terminal::{Cli, Result as TerminalResult, Terminal};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    db_path: String,
}

// make sure tresholds are ascending, do not put a lower value than the previous one
const CONFIRMATION_DEPTHS: &[u64] = &[5, 10, 20, 30, 50, 100];
// make sure max tracked blocks is greater than the last treshold, we add an extra
const MAX_TRACKED_BLOCKS: usize = 150;

struct FinalityMonitor {
    term: Arc<Mutex<Option<Arc<Terminal>>>>,
    db: Arc<DB>,
    storage: Arc<ConsensusStorage>,
    /// A snapshot of the virtual selected parent chain, from the sink backwards.
    vspc_snapshot: Arc<TokioMutex<VecDeque<Hash>>>,
    /// A persistent map of block hash to its highest processed confirmation depth threshold.
    block_hpt: Arc<TokioMutex<HashMap<Hash, u64>>>,
    /// Statistics on reorgs, mapping confirmation depth to reorg counts.
    reorg_stats: Arc<TokioMutex<HashMap<u64, u64>>>,
    /// Statistics on total blocks checked, mapping confirmation depth to counts.
    total_stats: Arc<TokioMutex<HashMap<u64, u64>>>,
    task_ctl: DuplexChannel,
    shutdown: Arc<AtomicBool>,
}

impl FinalityMonitor {
    fn try_new(db_path: &str) -> Result<Self, Box<dyn Error>> {
        let network = NetworkId::new(NetworkType::Mainnet);
        let config = Arc::new(ConfigBuilder::new(network.into()).adjust_perf_params_to_consensus_params().build());

        let db = ConnBuilder::default()
            .with_db_path(Path::new(db_path).to_path_buf())
            .with_files_limit(128)
            .build_secondary(Path::new(db_path).with_extension("secondary").to_path_buf())?;
        let storage = ConsensusStorage::new(db.clone(), config.clone());

        Ok(Self {
            term: Arc::new(Mutex::new(None)),
            db,
            storage,
            vspc_snapshot: Arc::new(TokioMutex::new(VecDeque::new())),
            block_hpt: Arc::new(TokioMutex::new(HashMap::new())),
            reorg_stats: Arc::new(TokioMutex::new(HashMap::new())),
            total_stats: Arc::new(TokioMutex::new(HashMap::new())),
            task_ctl: DuplexChannel::oneshot(),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    fn term(&self) -> Arc<Terminal> {
        self.term.lock().unwrap().as_ref().cloned().expect("Terminal not initialized")
    }

    fn start_monitoring_task(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                select! {
                    _ = this.task_ctl.request.receiver.recv().fuse() => {
                        break;
                    },
                    _ = sleep(Duration::from_millis(1000)).fuse() => {
                        if let Err(e) = this.update_and_draw().await {
                            warn!("Error in monitoring task: {}", e);
                        }
                    }
                }
            }
            this.task_ctl
                .response
                .sender
                .send(())
                .await
                .unwrap_or_else(|err| warn!("finality-monitor: unable to signal task shutdown: `{err}`"));
        });
    }

    async fn update_and_draw(&self) -> Result<(), Box<dyn Error>> {
        let term = self.term();

        if self.db.try_catch_up_with_primary().is_err() {
            term.writeln("Syncing with primary DB...");
            sleep(Duration::from_millis(200)).await;
            return Ok(());
        }

        let new_sink = match self.get_sink() {
            Ok(sink) => sink,
            Err(err) => {
                term.writeln("[2J[H");
                term.writeln(&format!("Error getting virtual store: {}", err));
                sleep(Duration::from_secs(1)).await;
                return Ok(());
            }
        };

        let mut vspc_snapshot = self.vspc_snapshot.lock().await;

        if vspc_snapshot.front().map_or(true, |h| *h != new_sink) {
            if let Err(e) = self.update_stats_for_new_sink(new_sink, &mut vspc_snapshot).await {
                warn!("Error updating finality stats: {}", e);
            }
        }

        drop(vspc_snapshot);

        self.draw_stats_table().await?;

        Ok(())
    }

    fn get_sink(&self) -> Result<Hash, kaspa_database::prelude::StoreError> {
        self.storage.virtual_stores.read().state.clone_with_new_cache().get().map(|vs| vs.ghostdag_data.selected_parent)
    }

    async fn update_stats_for_new_sink(&self, new_sink: Hash, vspc_snapshot: &mut VecDeque<Hash>) -> Result<(), Box<dyn Error>> {
        let new_sink_blue_score = match self.storage.ghostdag_store.get_blue_score(new_sink) {
            Ok(score) => score,
            Err(err) => {
                let term = self.term();
                term.writeln("[2J[H");
                term.writeln(&format!("Error getting blue score for new sink: {}", err));
                sleep(Duration::from_secs(1)).await;
                return Ok(());
            }
        };

        let new_snapshot = self.build_new_snapshot(new_sink);

        let is_first_iteration = vspc_snapshot.is_empty();
        if !is_first_iteration {
            let reorged_blocks = Self::find_reorged_blocks(vspc_snapshot, &new_snapshot);
            if !reorged_blocks.is_empty() {
                self.update_reorg_stats(reorged_blocks, new_sink_blue_score).await;
            }
        }

        self.update_total_stats_for_new_snapshot(&new_snapshot, new_sink_blue_score, is_first_iteration).await;

        *vspc_snapshot = new_snapshot;

        Ok(())
    }

    /// Builds a new snapshot of the selected parent chain, starting from the new sink.
    fn build_new_snapshot(&self, new_sink: Hash) -> VecDeque<Hash> {
        let mut new_snapshot: VecDeque<Hash> = VecDeque::with_capacity(MAX_TRACKED_BLOCKS);
        let mut current = new_sink;
        while new_snapshot.len() < MAX_TRACKED_BLOCKS {
            new_snapshot.push_back(current);
            if let Some(parent) = self.storage.ghostdag_store.get_selected_parent(current).unwrap_option() {
                current = parent;
            } else {
                // question: do we get there by using reasonable MAX_TRACKED_BLOCKS?
                break;
            }
        }
        new_snapshot
    }

    /// Compares the old and new snapshots to find blocks that were reorged out (of the vspc)
    fn find_reorged_blocks(vspc_snapshot: &mut VecDeque<Hash>, new_snapshot: &VecDeque<Hash>) -> Vec<Hash> {
        let new_hashes: HashSet<Hash> = new_snapshot.iter().copied().collect();
        let common_ancestor_idx_in_old = vspc_snapshot.iter().position(|h| new_hashes.contains(h));

        if let Some(old_idx) = common_ancestor_idx_in_old {
            vspc_snapshot.drain(0..old_idx).collect()
        } else {
            vspc_snapshot.drain(..).collect()
        }
    }

    async fn update_reorg_stats(&self, reorged_blocks: Vec<Hash>, new_sink_blue_score: u64) {
        let mut reorgs = self.reorg_stats.lock().await;
        let mut totals = self.total_stats.lock().await;
        let hpts = self.block_hpt.lock().await;

        for reorged_hash in reorged_blocks {
            let highest_processed_threshold = *hpts.get(&reorged_hash).unwrap_or(&0);

            let reorged_block_blue_score = match self.storage.ghostdag_store.get_blue_score(reorged_hash) {
                Ok(score) => score,
                Err(err) => {
                    warn!("finality-monitor: could not get blue score for reorged block {}: {}", reorged_hash, err);
                    continue;
                }
            };
            let confirmation_depth = new_sink_blue_score - reorged_block_blue_score;

            for &depth_threshold in CONFIRMATION_DEPTHS {
                if confirmation_depth >= depth_threshold {
                    *reorgs.entry(depth_threshold).or_insert(0) += 1;
                    if depth_threshold > highest_processed_threshold {
                        // this block passed a new threshold before being reorged, so we count it in totals for this threshold as well
                        // note: it hasn't been incremented in the new_snapshot discovery because it has been re-orged
                        *totals.entry(depth_threshold).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    /// Updates the total block counts for each confirmation depth threshold (reorged blocks get counted in update_reorg_stats).
    async fn update_total_stats_for_new_snapshot(
        &self,
        new_snapshot: &VecDeque<Hash>,
        new_sink_blue_score: u64,
        is_first_iteration: bool,
    ) {
        let mut totals = self.total_stats.lock().await;
        let mut hpts = self.block_hpt.lock().await;

        for hash in new_snapshot.iter() {
            let block_blue_score = match self.storage.ghostdag_store.get_blue_score(*hash) {
                Ok(score) => score,
                Err(err) => {
                    warn!("finality-monitor: could not get blue score for block {}: {}", hash, err);
                    continue;
                }
            };
            let confirmation_depth = new_sink_blue_score - block_blue_score;

            let old_hpt = *hpts.get(hash).unwrap_or(&0);
            let mut new_hpt = old_hpt;

            for &depth_threshold in CONFIRMATION_DEPTHS {
                if confirmation_depth >= depth_threshold {
                    new_hpt = new_hpt.max(depth_threshold);
                    if !is_first_iteration && depth_threshold > old_hpt {
                        *totals.entry(depth_threshold).or_insert(0) += 1;
                    }
                }
            }
            if new_hpt > old_hpt {
                hpts.insert(*hash, new_hpt);
            }
        }
    }

    /// Draws the finality statistics table in the terminal.
    async fn draw_stats_table(&self) -> Result<(), Box<dyn Error>> {
        let term = self.term();
        term.writeln("[2J[H");
        term.writeln(&format!("{:<18} | {:<22} | {:<12} | Total Checks", "Confirmation Depth", "Finality Probability", "Reorgs"));
        term.writeln("----------------------------------------------------------------------------");

        let reorgs = self.reorg_stats.lock().await;
        let totals = self.total_stats.lock().await;

        for &depth in CONFIRMATION_DEPTHS {
            let reorg_count = *reorgs.get(&depth).unwrap_or(&0);
            let total_checks = *totals.get(&depth).unwrap_or(&0);
            let probability = if total_checks > 0 { 1.0 - (reorg_count as f64 / total_checks as f64) } else { 1.0 };
            term.writeln(&format!("{:<18} | {:<21.4}% | {:<12} | {}", depth, probability * 100.0, reorg_count, total_checks));
        }

        Ok(())
    }
}

#[async_trait]
impl Cli for FinalityMonitor {
    fn init(self: Arc<Self>, term: &Arc<Terminal>) -> TerminalResult<()> {
        *self.term.lock().unwrap() = Some(term.clone());
        self.start_monitoring_task();
        Ok(())
    }

    async fn digest(self: Arc<Self>, term: Arc<Terminal>, cmd: String) -> TerminalResult<()> {
        if cmd.trim() == "quit" || cmd.trim() == "exit" {
            self.shutdown.store(true, Ordering::SeqCst);
            self.task_ctl.signal(()).await?;
            term.exit().await;
        } else if !cmd.trim().is_empty() {
            term.writeln("Unknown command. Type 'quit' or 'exit' to close.");
        }
        Ok(())
    }

    async fn complete(self: Arc<Self>, _term: Arc<Terminal>, _cmd: String) -> TerminalResult<Option<Vec<String>>> {
        Ok(None)
    }

    fn prompt(&self) -> Option<String> {
        if self.shutdown.load(Ordering::SeqCst) {
            None
        } else {
            Some("> ".to_string())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    kaspa_core::log::init_logger(None, "");

    let args = Args::parse();

    let monitor = Arc::new(FinalityMonitor::try_new(&args.db_path)?);
    let term = Arc::new(Terminal::try_new_with_options(monitor.clone(), workflow_terminal::Options::default())?);
    term.init().await?;
    term.writeln(&format!("Starting live finality analysis..."));
    term.writeln(&format!("Monitoring database at: {}", args.db_path));
    term.writeln("Type 'quit' or 'exit' to stop.");
    sleep(Duration::from_secs(2)).await;

    term.run().await?;

    Ok(())
}
