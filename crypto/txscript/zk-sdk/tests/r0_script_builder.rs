use kaspa_consensus_core::{
    hashing::sighash::SigHashReusedValuesUnsync,
    subnets::SubnetworkId,
    tx::{PopulatedTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry},
};
use kaspa_hashes::Hash;
use kaspa_txscript::{EngineCtx, EngineFlags, SigCacheKey, TxScriptEngine, caches::Cache, hex, pay_to_script_hash_script};
use kaspa_txscript_errors::TxScriptError;
use kaspa_txscript_zk_sdk::R0ScriptBuilder;
use risc0_zkvm::{Digest, Groth16Receipt, ReceiptClaim, SuccinctReceipt};

fn execute_p2sh(sig_script: Vec<u8>, redeem_script: &[u8]) -> Result<(), TxScriptError> {
    let spk = pay_to_script_hash_script(redeem_script);

    let dummy_outpoint = TransactionOutpoint::new(Hash::from_u64_word(0), 0);
    let input = TransactionInput::new(dummy_outpoint, sig_script, 0, 0);
    let output = TransactionOutput::new(1_000_000, spk.clone());
    let mut tx = Transaction::new(0, vec![input], vec![output], 0, SubnetworkId::default(), 0, vec![]);
    tx.finalize();

    let utxo_entry = UtxoEntry::new(1_000_000, spk, 0, false, None);
    let sig_cache: Cache<SigCacheKey, bool> = Cache::new(10_000);
    let reused_values = SigHashReusedValuesUnsync::new();
    let populated = PopulatedTransaction::new(&tx, vec![utxo_entry]);
    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &tx.inputs[0],
        0,
        &populated.entries[0],
        EngineCtx::new(&sig_cache).with_reused(&reused_values),
        EngineFlags { covenants_enabled: true, ..Default::default() },
    );
    vm.execute()
}

#[test]
fn r0_script_builder_groth16_verifies() {
    let journal_hash: [u8; 32] =
        hex::decode("5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456").unwrap().try_into().unwrap();
    let image_id: [u8; 32] =
        hex::decode("75641a540ee2ad9ee5902bcdcdb8b55c0bef4a28287309b858f97b1356c6c2e0").unwrap().try_into().unwrap();

    let receipt_raw = include_str!("data/zk_builder_tests/groth.rcpt.hex");
    let receipt: Groth16Receipt<ReceiptClaim> = borsh::from_slice(&hex::decode(receipt_raw).unwrap()).unwrap();

    let finalized = R0ScriptBuilder::with_flags(EngineFlags { covenants_enabled: true, ..Default::default() })
        .commit_to_groth16(image_id)
        .unwrap()
        .finalize_with_proof(receipt, journal_hash)
        .unwrap();

    execute_p2sh(finalized.sig_script, &finalized.redeem_script).unwrap();
}

#[test]
fn r0_script_builder_groth16_binds_image_id() {
    let journal_hash: [u8; 32] =
        hex::decode("5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456").unwrap().try_into().unwrap();
    let image_id: [u8; 32] =
        hex::decode("70641a540ee2ad9ee5902bcdcdb8b55c0bef4a28287309b858f97b1356c6c2e0").unwrap().try_into().unwrap();

    let receipt_raw = include_str!("data/zk_builder_tests/groth.rcpt.hex");
    let receipt: Groth16Receipt<ReceiptClaim> = borsh::from_slice(&hex::decode(receipt_raw).unwrap()).unwrap();

    let finalized = R0ScriptBuilder::with_flags(EngineFlags { covenants_enabled: true, ..Default::default() })
        .commit_to_groth16(image_id)
        .unwrap()
        .finalize_with_proof(receipt, journal_hash)
        .unwrap();

    assert!(matches!(execute_p2sh(finalized.sig_script, &finalized.redeem_script), Err(TxScriptError::ZkIntegrity(_))));
}

#[test]
fn r0_script_builder_groth16_binds_journal_hash() {
    let journal_hash: [u8; 32] =
        hex::decode("6df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456").unwrap().try_into().unwrap();
    let image_id: [u8; 32] =
        hex::decode("75641a540ee2ad9ee5902bcdcdb8b55c0bef4a28287309b858f97b1356c6c2e0").unwrap().try_into().unwrap();

    let receipt_raw = include_str!("data/zk_builder_tests/groth.rcpt.hex");
    let receipt: Groth16Receipt<ReceiptClaim> = borsh::from_slice(&hex::decode(receipt_raw).unwrap()).unwrap();

    let finalized = R0ScriptBuilder::with_flags(EngineFlags { covenants_enabled: true, ..Default::default() })
        .commit_to_groth16(image_id)
        .unwrap()
        .finalize_with_proof(receipt, journal_hash)
        .unwrap();

    assert!(matches!(execute_p2sh(finalized.sig_script, &finalized.redeem_script), Err(TxScriptError::ZkIntegrity(_))));
}

#[test]
fn r0_script_builder_succinct_verifies() {
    let receipt_raw = include_str!("data/zk_builder_tests/succinct.rcpt.hex");
    let image_id_raw = include_str!("data/zk_builder_tests/succinct.image.hex");
    let journal_raw = include_str!("data/zk_builder_tests/succinct.journal.hex");
    let image_id: Digest = hex::decode(image_id_raw).unwrap().try_into().unwrap();
    let journal: Digest = hex::decode(journal_raw).unwrap().try_into().unwrap();
    let receipt: SuccinctReceipt<ReceiptClaim> = borsh::from_slice(&hex::decode(receipt_raw).unwrap()).unwrap();

    let finalized = R0ScriptBuilder::with_flags(EngineFlags { covenants_enabled: true, ..Default::default() })
        .commit_to_succinct(image_id.as_bytes().try_into().unwrap(), receipt.control_id.as_bytes().try_into().unwrap(), None)
        .unwrap()
        .finalize_with_proof(receipt, journal)
        .unwrap();

    execute_p2sh(finalized.sig_script, &finalized.redeem_script).unwrap();
}
