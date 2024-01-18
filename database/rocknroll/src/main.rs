#![allow(dead_code)]
#![allow(unused_imports)]

use std::{mem::size_of, sync::Arc};

use kaspa_consensus::consensus::storage::ConsensusStorage;
use kaspa_consensus_core::{
    config::ConfigBuilder,
    network::{NetworkId, NetworkType},
    tx::{ScriptVec, TransactionOutpoint, UtxoEntry},
};
use kaspa_core::info;
use kaspad_lib::daemon::{get_app_dir, CONSENSUS_DB, DEFAULT_DATA_DIR, META_DB, UTXOINDEX_DB};

fn to_human_readable(mut number_to_format: f64, precision: usize, suffix: &str) -> String {
    const UNITS: [&str; 7] = ["", "K", "M", "G", "T", "P", "E"];
    const DIV: [f64; 7] =
        [1.0, 1_000.0, 1_000_000.0, 1_000_000_000.0, 1_000_000_000_000.0, 1_000_000_000_000_000.0, 1_000_000_000_000_000_000.0];
    let i = (number_to_format.log(1000.0) as usize).min(UNITS.len() - 1);
    number_to_format /= DIV[i];
    format!("{number_to_format:.precision$}{}{}", UNITS[i], suffix)
}

fn main() {
    kaspa_core::log::init_logger(None, "");
    let network = NetworkId::with_suffix(NetworkType::Testnet, 11);
    let app_dir = get_app_dir();
    let db_dir = app_dir.join(network.to_prefixed()).join(DEFAULT_DATA_DIR);
    let consensus_db_dir = db_dir.join(CONSENSUS_DB).join("consensus-001");
    // let utxoindex_db_dir = db_dir.join(UTXOINDEX_DB);
    // let meta_db_dir = db_dir.join(META_DB);

    let config = Arc::new(ConfigBuilder::new(network.into()).adjust_perf_params_to_consensus_params().build());
    let db =
        kaspa_database::prelude::ConnBuilder::default().with_db_path(consensus_db_dir).with_files_limit(128).build_readonly().unwrap();

    let storage = ConsensusStorage::new(db, config);
    let mut count = 0;
    let mut bytes = 0;
    for (_, entry) in storage.pruning_utxoset_stores.read().utxo_set.iterator().map(|p| p.unwrap()) {
        count += 1;
        bytes += size_of::<TransactionOutpoint>();
        bytes += size_of::<UtxoEntry>() - size_of::<ScriptVec>() + entry.script_public_key.script().len();
    }
    info!("UTXO set count: {}, size: {}", count, to_human_readable(bytes as f64, 3, "B"));

    let full_blocks = storage.block_transactions_store.iterator().count();
    dbg!(full_blocks);

    // drop(db);
}
