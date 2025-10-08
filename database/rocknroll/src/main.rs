#![allow(dead_code)]
#![allow(unused_imports)]

use chrono::{TimeZone, Utc};
use std::{
    mem::size_of,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use kaspa_consensus::{
    consensus::{services::ConsensusServices, storage::ConsensusStorage},
    model::stores::{
        acceptance_data::AcceptanceDataStoreReader, block_transactions::BlockTransactionsStoreReader, headers::HeaderStoreReader,
        pruning::PruningStoreReader, utxo_diffs::UtxoDiffsStoreReader,
    },
};
use kaspa_consensus_core::{
    acceptance_data::MergesetBlockAcceptanceData,
    block,
    config::ConfigBuilder,
    network::{NetworkId, NetworkType},
    tx::{ScriptVec, SignableTransaction, Transaction, TransactionOutpoint, UtxoEntry},
    utxo::utxo_diff::ImmutableUtxoDiff,
    Hash,
};
use kaspa_core::info;
use kaspad_lib::daemon::{get_app_dir, CONSENSUS_DB, DEFAULT_DATA_DIR, META_DB, UTXOINDEX_DB};

fn main() {
    kaspa_core::log::init_logger(None, "");
    let network = NetworkId::new(NetworkType::Mainnet);
    let app_dir = get_app_dir();
    let db_dir = app_dir.join(network.to_prefixed()).join(DEFAULT_DATA_DIR);
    let consensus_db_dir = db_dir.join(CONSENSUS_DB).join("consensus-002"); // check your own index
                                                                            // let utxoindex_db_dir = db_dir.join(UTXOINDEX_DB);
                                                                            // let meta_db_dir = db_dir.join(META_DB);

    let config = Arc::new(ConfigBuilder::new(network.into()).adjust_perf_params_to_consensus_params().build());
    let db =
        kaspa_database::prelude::ConnBuilder::default().with_db_path(consensus_db_dir).with_files_limit(128).build_readonly().unwrap();

    let storage = ConsensusStorage::new(db.clone(), config.clone());
    let services = ConsensusServices::new(db, storage.clone(), config, Default::default(), Default::default());

    let start_datetime = Utc.with_ymd_and_hms(2025, 10, 5, 0, 0, 0).unwrap();
    let end_datetime = Utc.with_ymd_and_hms(2025, 10, 6, 0, 0, 0).unwrap();
    let start: SystemTime = start_datetime.into();
    let end: SystemTime = end_datetime.into();

    let pp = storage.pruning_point_store.read().pruning_point().unwrap();
    let sink = storage.lkg_virtual_state.load().ghostdag_data.selected_parent;

    let (start, end) =
        (start.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64, end.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64);
    let mut count = 0;

    let mut fee = 0;

    for cb in services.reachability_service.forward_chain_iterator(pp, sink, false) {
        let timestamp = storage.headers_store.get_timestamp(cb).unwrap();
        if start <= timestamp && timestamp < end {
            let ad = storage.acceptance_data_store.get(cb).unwrap();
            let (cb_accepted_fees, mergeset_accepted_txs_count) = ad
                .iter()
                .map(|d| {
                    let cb_accepted_fees = calc_fees_in_cb(cb, d, ad.clone(), storage.clone());

                    (cb_accepted_fees, d.accepted_transactions.len())
                })
                .reduce(|(a, b), (c, d)| (a + c, b + d))
                .unwrap();

            fee += cb_accepted_fees;
            count += mergeset_accepted_txs_count;
            if (count - mergeset_accepted_txs_count) / 10_000_000 != count / 10_000_000 {
                info!("Accepted txs in range: {}", count);
                info!("Fees paid: {}", fee);
            }
        }
    }
    info!(
        "\n=======================================\n\tAccepted txs in range {} - {}: {}\n=======================================",
        start_datetime.format("%d/%m/%Y %H:%M"),
        end_datetime.format("%d/%m/%Y %H:%M"),
        count
    );
    info!("Fees paid: {}", fee);
}

fn calc_fees_in_cb(
    cb: Hash,
    d: &MergesetBlockAcceptanceData,
    ad: Arc<Vec<MergesetBlockAcceptanceData>>,
    storage: Arc<ConsensusStorage>,
) -> u64 {
    let block_txs = storage.block_transactions_store.get(d.block_hash).unwrap();

    d.accepted_transactions
        .iter()
        .map(|h| {
            let utxo_diff = storage.utxo_diffs_store.get(cb).unwrap();

            let tx = find_tx_from_block_txs_with_idx(h.transaction_id, h.index_within_block, block_txs.clone());

            let removed_diffs = utxo_diff.removed();

            let in_sum = tx
                .inputs
                .iter()
                .map(|ti| {
                    if let Some(utxo_entry) = removed_diffs.get(&ti.previous_outpoint) {
                        utxo_entry.amount
                    } else {
                        // This handles this rare scenario:
                        // - UTXO0 is spent by TX1 and creates UTXO1
                        // - UTXO1 is spent by TX2 and creates UTXO2
                        // - A chain block happens to accept both of these
                        // In this case, removed_diff wouldn't contain the outpoint of the created-and-immediately-spent UTXO
                        // so we use the transaction (which also has acceptance data in this block) and look at its outputs
                        let other_txid = ti.previous_outpoint.transaction_id;
                        let other_tx = find_tx_from_acceptance(other_txid, ad.clone(), storage.clone());
                        assert_eq!(other_tx.id(), other_txid, "expected to find the correct other_txid");
                        let output = &other_tx.outputs[ti.previous_outpoint.index as usize];
                        output.value
                    }
                })
                .sum::<u64>();
            let out_sum = tx.outputs.iter().map(|to| to.value).sum::<u64>();

            // Saturating sub to cover the coinbase case and make that return 0
            in_sum.saturating_sub(out_sum)
        })
        .sum::<u64>()
}

fn find_tx_from_acceptance(
    txid: Hash,
    acceptance_data: Arc<Vec<MergesetBlockAcceptanceData>>,
    storage: Arc<ConsensusStorage>,
) -> Transaction {
    let (block_hash, idx_in_block) = acceptance_data
        .iter()
        .find_map(|d| {
            d.accepted_transactions.iter().find_map(|a| (a.transaction_id == txid).then_some((d.block_hash, a.index_within_block)))
        })
        .unwrap();

    let block_txs = storage.block_transactions_store.get(block_hash).unwrap();

    find_tx_from_block_txs_with_idx(txid, idx_in_block, block_txs)
}

fn find_tx_from_block_txs_with_idx(txid: Hash, idx_in_block: u32, block_txs: Arc<Vec<Transaction>>) -> Transaction {
    let found_tx = block_txs.get(idx_in_block as usize).unwrap();
    assert_eq!(txid, found_tx.id(), "{} != {}", txid, found_tx.id());

    found_tx.to_owned()
}
