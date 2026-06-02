use crate::{
    hashing::HasherExtensions,
    tx::{TransactionOutpoint, UtxoEntry, VerifiableTransaction},
};
use kaspa_hashes::HasherBase;
use kaspa_muhash::MuHash;

pub trait MuHashExtensions {
    fn add_transaction(&mut self, tx: &impl VerifiableTransaction, block_daa_score: u64);
    fn add_utxo(&mut self, outpoint: &TransactionOutpoint, entry: &UtxoEntry);
    fn remove_utxo(&mut self, outpoint: &TransactionOutpoint, entry: &UtxoEntry);
    fn from_transaction(tx: &impl VerifiableTransaction, block_daa_score: u64) -> Self;
    fn from_utxo(outpoint: &TransactionOutpoint, entry: &UtxoEntry) -> Self;
}

impl MuHashExtensions for MuHash {
    fn add_transaction(&mut self, tx: &impl VerifiableTransaction, block_daa_score: u64) {
        let tx_id = tx.id();
        for (input, entry) in tx.populated_inputs() {
            let mut writer = self.remove_element_builder();
            write_utxo(&mut writer, entry, &input.previous_outpoint);
            writer.finalize();
        }
        for (i, output) in tx.outputs().iter().enumerate() {
            let outpoint = TransactionOutpoint::new(tx_id, i as u32);
            let entry = UtxoEntry::new(
                output.value,
                output.script_public_key.clone(),
                block_daa_score,
                tx.is_coinbase(),
                output.covenant.map(|info| info.covenant_id),
            );
            self.add_utxo(&outpoint, &entry);
        }
    }

    fn add_utxo(&mut self, outpoint: &TransactionOutpoint, entry: &UtxoEntry) {
        let mut writer = self.add_element_builder();
        write_utxo(&mut writer, entry, outpoint);
        writer.finalize();
    }

    fn remove_utxo(&mut self, outpoint: &TransactionOutpoint, entry: &UtxoEntry) {
        let mut writer = self.remove_element_builder();
        write_utxo(&mut writer, entry, outpoint);
        writer.finalize();
    }

    fn from_transaction(tx: &impl VerifiableTransaction, block_daa_score: u64) -> Self {
        let mut mh = Self::new();
        mh.add_transaction(tx, block_daa_score);
        mh
    }

    fn from_utxo(outpoint: &TransactionOutpoint, entry: &UtxoEntry) -> Self {
        let mut mh = Self::new();
        mh.add_utxo(outpoint, entry);
        mh
    }
}

fn write_utxo(writer: &mut impl HasherBase, entry: &UtxoEntry, outpoint: &TransactionOutpoint) {
    writer
        // Outpoint
        .update(outpoint.transaction_id)
        .update(outpoint.index.to_le_bytes())
        // Utxo entry
        .update(entry.block_daa_score.to_le_bytes())
        .update(entry.amount.to_le_bytes())
        .write_bool(entry.is_coinbase)
        .update(entry.script_public_key.version().to_le_bytes())
        .write_var_bytes(entry.script_public_key.script());
    if let Some(covenant_id) = entry.covenant_id {
        writer.update(covenant_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::{ScriptPublicKey, TransactionOutpoint, UtxoEntry};
    use kaspa_hashes::Hash;

    fn utxo(i: u64) -> (TransactionOutpoint, UtxoEntry) {
        let outpoint = TransactionOutpoint::new(Hash::from_u64_word(i), (i % 4) as u32);
        let entry = UtxoEntry::new(1_000 + i, ScriptPublicKey::default(), i, i.is_multiple_of(2), None);
        (outpoint, entry)
    }

    /// `remove_utxo` is the exact group inverse of `add_utxo`: adding then removing the same utxo leaves the
    /// empty multiset. This underpins folding a spend out of the incremental pruning-point MuHash.
    #[test]
    fn remove_utxo_inverts_add_utxo() {
        let (outpoint, entry) = utxo(7);
        let mut mh = MuHash::new();
        mh.add_utxo(&outpoint, &entry);
        mh.remove_utxo(&outpoint, &entry);
        assert_eq!(mh.finalize(), MuHash::new().finalize(), "add then remove must yield the empty multiset");
    }

    /// The incrementally-folded MuHash (a sequence of adds and removes) equals the full recompute over the
    /// resulting surviving set. This is the commutative-group equivalence the incremental pruning-point
    /// commitment relies on: order is immaterial, so the folded value is provably the full-set value.
    #[test]
    fn incremental_fold_equals_full_recompute() {
        let utxos: Vec<_> = (0..32).map(utxo).collect();
        let spent: Vec<usize> = vec![3, 11, 19, 27, 5];

        // Incremental: add every utxo, then spend (remove) a subset -- the create/spend pattern of real diffs.
        let mut incremental = MuHash::new();
        for (outpoint, entry) in &utxos {
            incremental.add_utxo(outpoint, entry);
        }
        for &i in &spent {
            incremental.remove_utxo(&utxos[i].0, &utxos[i].1);
        }

        // Full recompute over only the surviving set.
        let mut full = MuHash::new();
        for (i, (outpoint, entry)) in utxos.iter().enumerate() {
            if !spent.contains(&i) {
                full.add_utxo(outpoint, entry);
            }
        }

        assert_eq!(incremental.finalize(), full.finalize(), "incremental fold must equal the full recompute of the surviving set");
    }
}
