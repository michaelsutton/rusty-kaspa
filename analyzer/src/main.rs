#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unreachable_code)]

use async_channel::unbounded;
use clap::Parser;
use futures::{future::try_join_all, Future};
use itertools::Itertools;
use kaspa_consensus::{
    config::ConfigBuilder,
    consensus::Consensus,
    constants::perf::PerfParams,
    model::stores::{
        acceptance_data::AcceptanceDataStoreReader,
        block_transactions::BlockTransactionsStoreReader,
        ghostdag::{GhostdagStoreReader, KType},
        headers::HeaderStoreReader,
        relations::RelationsStoreReader,
        virtual_state::VirtualStateStoreReader,
    },
    params::{Params, Testnet11Bps, DEVNET_PARAMS, TESTNET11_PARAMS},
};
use kaspa_consensus_core::{
    api::ConsensusApi, block::Block, blockstatus::BlockStatus, errors::block::BlockProcessResult, BlockHashSet, HashMapCustomHasher,
};
use kaspa_consensus_notify::root::ConsensusNotificationRoot;
use kaspa_core::{info, warn};
use kaspa_database::utils::{create_temp_db_with_parallelism, load_existing_db};
use kaspa_hashes::Hash;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, fs::File, io::Write, ops::Deref, path::Path, sync::Arc};

/// Kaspa Network Simulator
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Simulation blocks per second
    #[arg(short, long, default_value_t = 1.0)]
    bps: f64,

    /// Simulation delay (seconds)
    #[arg(short, long, default_value_t = 2.0)]
    delay: f64,

    /// Number of miners
    #[arg(short, long, default_value_t = 1)]
    miners: u64,

    /// Target transactions per block
    #[arg(short, long, default_value_t = 200)]
    tpb: u64,

    /// Target simulation time (seconds)
    #[arg(short, long, default_value_t = 600)]
    sim_time: u64,

    /// Target number of blocks the simulation should produce (overrides --sim-time if specified)
    #[arg(short = 'n', long)]
    target_blocks: Option<u64>,

    /// Number of pool-thread threads used by the header and body processors.
    /// Defaults to the number of logical CPU cores.
    #[arg(short, long)]
    processors_threads: Option<usize>,

    /// Number of pool-thread threads used by the virtual processor (for parallel transaction verification).
    /// Defaults to the number of logical CPU cores.
    #[arg(short, long)]
    virtual_threads: Option<usize>,

    /// Logging level for all subsystems {off, error, warn, info, debug, trace}
    ///  -- You may also specify <subsystem>=<level>,<subsystem2>=<level>,... to set the log level for individual subsystems
    #[arg(long = "loglevel", default_value = format!("info,{}=trace", env!("CARGO_PKG_NAME")))]
    log_level: String,

    /// Output directory to save the simulation DB
    #[arg(short, long)]
    output_dir: Option<String>,

    /// Input directory of an existing consensus
    #[arg(short, long)]
    input_dir: Option<String>,

    /// Indicates whether to test pruning. Currently this means we shorten the pruning constants and avoid validating
    /// the DAG in a separate consensus following the simulation phase
    #[arg(long, default_value_t = false)]
    test_pruning: bool,

    /// Use the legacy full-window DAA mechanism (note: the size of this window scales with bps)
    #[arg(long, default_value_t = false)]
    daa_legacy: bool,

    /// Use testnet-11 consensus params
    #[arg(long, default_value_t = false)]
    testnet11: bool,
}

fn main() {
    // Get CLI arguments
    let mut args = Args::parse();

    // Initialize the logger
    kaspa_core::log::init_logger(None, &args.log_level);

    // Print package name and version
    info!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    // Configure the panic behavior
    kaspa_core::panic::configure_panic();

    assert!(args.bps * args.delay < 250.0, "The delay times bps product is larger than 250");
    if args.miners > 1 {
        warn!(
            "Warning: number of miners was configured to {}. Currently each miner added doubles the simulation 
        memory and runtime footprint, while a single miner is sufficient for most simulation purposes (delay is simulated anyway).",
            args.miners
        );
    }
    args.bps = if args.testnet11 { Testnet11Bps::bps() as f64 } else { args.bps };
    let params = if args.testnet11 { TESTNET11_PARAMS } else { DEVNET_PARAMS };
    let mut builder = ConfigBuilder::new(params)
        .apply_args(|config| apply_args_to_perf_params(&args, &mut config.perf))
        .adjust_perf_params_to_consensus_params()
        .enable_sanity_checks()
        .skip_adding_genesis();
    if !args.test_pruning {
        builder = builder.set_archival();
    }
    let config = Arc::new(builder.build());

    // Load an existing consensus or run the simulation
    let (consensus, _lifetime) = if let Some(input_dir) = args.input_dir {
        let (lifetime, db) = load_existing_db(input_dir, num_cpus::get());
        let (dummy_notification_sender, _) = unbounded();
        let notification_root = Arc::new(ConsensusNotificationRoot::new(dummy_notification_sender));
        let consensus = Arc::new(Consensus::new(db, config.clone(), Default::default(), notification_root, Default::default()));
        (consensus, lifetime)
    } else {
        return;
    };

    if args.test_pruning {
        drop(consensus);
        return;
    }

    // Benchmark the DAG validation time
    let (_lifetime2, db2) = create_temp_db_with_parallelism(num_cpus::get());
    let (dummy_notification_sender, _) = unbounded();
    let notification_root = Arc::new(ConsensusNotificationRoot::new(dummy_notification_sender));
    let consensus2 = Arc::new(Consensus::new(db2, config.clone(), Default::default(), notification_root, Default::default()));
    let handles2 = consensus2.run_processors();
    validate(&consensus, &consensus2, &config, args.bps);
    consensus2.shutdown(handles2);
    drop(consensus);
}

fn apply_args_to_perf_params(args: &Args, perf_params: &mut PerfParams) {
    if let Some(processors_pool_threads) = args.processors_threads {
        perf_params.block_processors_num_threads = processors_pool_threads;
    }
    if let Some(virtual_pool_threads) = args.virtual_threads {
        perf_params.virtual_processor_num_threads = virtual_pool_threads;
    }
}

#[tokio::main]
async fn validate(src_consensus: &Consensus, dst_consensus: &Consensus, params: &Params, bps: f64) {
    save_to_json(src_consensus, params.genesis.hash, "/home/pool/michael/data/testnet11-dag-dump.json");
    return;

    tx_efficiency(src_consensus, params.genesis.hash);
    return;

    let hashes = topologically_ordered_hashes(src_consensus, params.genesis.hash);
    let num_blocks = hashes.len();
    let num_txs = print_stats(src_consensus, &hashes, bps, params.ghostdag_k);
    info!("Validating {num_blocks} blocks with {num_txs} transactions overall...");
    // let start = std::time::Instant::now();
    // let chunks = hashes.into_iter().chunks(1000);
    // let mut iter = chunks.into_iter();
    // let mut chunk = iter.next().unwrap();
    // let mut prev_joins = submit_chunk(src_consensus, dst_consensus, &mut chunk);

    // for mut chunk in iter {
    //     let current_joins = submit_chunk(src_consensus, dst_consensus, &mut chunk);
    //     let statuses = try_join_all(prev_joins).await.unwrap();
    //     assert!(statuses.iter().all(|s| s.is_utxo_valid_or_pending()));
    //     prev_joins = current_joins;
    // }

    // let statuses = try_join_all(prev_joins).await.unwrap();
    // assert!(statuses.iter().all(|s| s.is_utxo_valid_or_pending()));

    // // Assert that at least one body tip was resolved with valid UTXO
    // assert!(dst_consensus.body_tips().iter().copied().any(|h| dst_consensus.block_status(h) == BlockStatus::StatusUTXOValid));
    // let elapsed = start.elapsed();
    // info!(
    //     "Total validation time: {:?}, block processing rate: {:.2} (b/s), transaction processing rate: {:.2} (t/s)",
    //     elapsed,
    //     num_blocks as f64 / elapsed.as_secs_f64(),
    //     num_txs as f64 / elapsed.as_secs_f64(),
    // );
}

fn submit_chunk(
    src_consensus: &Consensus,
    dst_consensus: &Consensus,
    chunk: &mut impl Iterator<Item = Hash>,
) -> Vec<impl Future<Output = BlockProcessResult<BlockStatus>>> {
    let mut futures = Vec::new();
    for hash in chunk {
        let block = Block::from_arcs(
            src_consensus.headers_store.get_header(hash).unwrap(),
            src_consensus.block_transactions_store.get(hash).unwrap(),
        );
        let f = dst_consensus.validate_and_insert_block(block);
        futures.push(f);
    }
    futures
}

fn tx_efficiency(consensus: &Consensus, genesis_hash: Hash) {
    let sink = consensus.get_sink();
    let (mut total_txs, mut accepted_txs) = (0, 0);
    let (mut epoch_txs, mut epoch_accepted_txs) = (0, 0);
    for (i, cb) in consensus.services.reachability_service.default_backward_chain_iterator(sink).enumerate() {
        let ad = consensus.acceptance_data_store.get(cb).unwrap();
        let blues: BlockHashSet = consensus.ghostdag_primary_store.get_mergeset_blues(cb).unwrap().iter().copied().collect();
        for (j, mbad) in ad.iter().enumerate() {
            if !blues.contains(&mbad.block_hash) {
                continue;
            }
            let mbtx = consensus.block_transactions_store.get(mbad.block_hash).unwrap();
            total_txs += if j == 0 { mbtx.len() } else { mbtx.len() - 1 };
            epoch_txs += if j == 0 { mbtx.len() } else { mbtx.len() - 1 };
            accepted_txs += mbad.accepted_transactions.len();
            epoch_accepted_txs += mbad.accepted_transactions.len();
        }
        if (i + 1) % 2000 == 0 {
            info!(
                "Tx efficiency: {:.4}, epoch: {:.4}",
                accepted_txs as f64 / total_txs as f64,
                epoch_accepted_txs as f64 / epoch_txs as f64
            );
            epoch_txs = 0;
            epoch_accepted_txs = 0;
        }
    }

    info!("{}, {}, {}", accepted_txs, total_txs, accepted_txs as f64 / total_txs as f64);
}

fn topologically_ordered_hashes(src_consensus: &Consensus, genesis_hash: Hash) -> Vec<Hash> {
    let mut queue: VecDeque<Hash> = std::iter::once(genesis_hash).collect();
    let mut visited = BlockHashSet::new();
    let mut vec = Vec::new();
    let relations = src_consensus.relations_stores.read();
    let mut count = 0;
    while let Some(current) = queue.pop_front() {
        for child in relations[0].get_children(current).unwrap().iter() {
            if visited.insert(*child) {
                queue.push_back(*child);
                vec.push(*child);
                count += 1;
                if count % 5000 == 0 {
                    info!("Travesed {} blocks", count);
                }
            }
        }
    }
    // info!("Sorting...");
    // vec.sort_by_cached_key(|&h| src_consensus.ghostdag_primary_store.get_blue_work(h).unwrap());
    vec
}

fn print_stats(src_consensus: &Consensus, hashes: &[Hash], bps: f64, k: KType) -> usize {
    info!("Collecting stats for {} blocks...", hashes.len());
    let blues_mean =
        hashes.iter().map(|&h| src_consensus.ghostdag_primary_store.get_data(h).unwrap().mergeset_blues.len()).sum::<usize>() as f64
            / hashes.len() as f64;
    info!("blues: {}", blues_mean);
    let reds_mean =
        hashes.iter().map(|&h| src_consensus.ghostdag_primary_store.get_data(h).unwrap().mergeset_reds.len()).sum::<usize>() as f64
            / hashes.len() as f64;
    info!("reds: {}", reds_mean);
    let parents_mean = hashes.iter().map(|&h| src_consensus.headers_store.get_header(h).unwrap().direct_parents().len()).sum::<usize>()
        as f64
        / hashes.len() as f64;
    info!("parents: {}", parents_mean);
    let num_txs =
        hashes.iter().map(|&h| src_consensus.block_transactions_store.get(h).map(|v| v.len()).unwrap_or_default()).sum::<usize>();
    let txs_mean = num_txs as f64 / hashes.len() as f64; // TODO
    info!("[BPS={bps}, GHOSTDAG K={k}]");
    info!("[Average stats of DAG] blues: {blues_mean}, reds: {reds_mean}, parents: {parents_mean}, txs: {txs_mean}");
    num_txs
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonBlock {
    id: String,
    blue: bool,
    parents: Vec<String>,
}

fn save_to_json(consensus: &Consensus, genesis_hash: Hash, file_path: &str) {
    let mut file = File::options().write(true).create(true).truncate(true).open(Path::new(file_path)).unwrap();
    // let encoder = GzEncoder::new(file);

    let sink = consensus.get_sink();
    let relations_read = consensus.relations_stores.read();
    for (i, cb) in consensus.services.reachability_service.default_backward_chain_iterator(sink).skip(20000).enumerate() {
        let gd = consensus.ghostdag_primary_store.get_data(cb).unwrap();
        let blues: BlockHashSet = gd.mergeset_blues.iter().copied().collect();
        for b in gd.consensus_ordered_mergeset(consensus.ghostdag_primary_store.deref()) {
            let parents = relations_read[0].get_parents(b).unwrap();
            let jb =
                JsonBlock { id: b.to_string(), blue: blues.contains(&b), parents: parents.iter().map(|h| h.to_string()).collect() };
            let sb = serde_json::to_string(&jb).unwrap();
            writeln!(file, "{}", sb).unwrap();
        }

        if i > 120 {
            break;
        }
    }
}
