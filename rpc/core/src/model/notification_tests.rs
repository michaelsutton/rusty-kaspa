use super::{RpcHash, RpcUtxosByAddressesEntry, UtxosChangedNotification};
use crate::Notification as RpcNotification;
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionOutpoint};
use kaspa_index_core::{
    indexed_utxos::{CompactUtxoEntry, UtxoEntryKeySuffixRecord, UtxoSetByScriptPublicKey},
    notification::{Notification as IndexNotification, UtxosChangedNotification as IndexUtxosChanged},
};
use kaspa_notify::{
    notification::Notification as _,
    scope::{Scope, UtxosChangedFilteredScope, UtxosChangedScope},
    subscription::{
        Command, DynSubscription, MutateSingle, Mutation, MutationPolicies,
        context::SubscriptionContext,
        single::{UtxosChangedState, UtxosChangedSubscription},
    },
};
use kaspa_txscript::pay_to_address_script;
use std::{collections::HashSet, sync::Arc};

#[test]
fn address_filtering_paths_survive_covenant_subscription_changes() {
    let addresses: Vec<_> = (0..4).map(|i| Address::new(Prefix::Mainnet, Version::PubKey, &[i; 32])).collect();
    let [a, b] = [101, 102].map(RpcHash::from_u64_word);
    let mut added = UtxoSetByScriptPublicKey::new();
    for (address, tx, covenant_id) in [(0, 1, Some(a)), (0, 2, None), (1, 3, Some(a)), (1, 4, Some(b))] {
        added.entry(pay_to_address_script(&addresses[address])).or_default().insert(
            UtxoEntryKeySuffixRecord::new(20, TransactionOutpoint::new(RpcHash::from_u64_word(tx), 0)),
            CompactUtxoEntry::new(tx * 100, false, covenant_id),
        );
    }
    let notification = IndexUtxosChanged { added: Arc::new(added), removed: Arc::new(UtxoSetByScriptPublicKey::new()) };
    let original = UtxosChangedNotification::from(&notification);
    let context = SubscriptionContext::new();
    let single = Arc::new(UtxosChangedSubscription::new(UtxosChangedState::None, 1));
    let mut subscription: DynSubscription = single.clone();
    let address_scope =
        |indexes: &[usize]| -> Scope { UtxosChangedScope::new(indexes.iter().map(|&i| addresses[i].clone()).collect()).into() };
    let covenant_scope = |ids| -> Scope { UtxosChangedFilteredScope::new(vec![], ids).into() };
    let all = || -> Scope { UtxosChangedScope::default().into() };
    let steps: &[(Command, Scope, &[u64], bool)] = &[
        // Two buckets: first iterate one watched SPK, then the smaller bucket map.
        (Command::Start, address_scope(&[0]), &[1, 2], false),
        (Command::Start, address_scope(&[2, 3]), &[1, 2], false),
        (Command::Start, covenant_scope(vec![a, a]), &[1, 2, 3], true),
        (Command::Start, covenant_scope(vec![a]), &[1, 2, 3], true),
        (Command::Start, covenant_scope(vec![b]), &[1, 2, 3, 4], true),
        (Command::Stop, covenant_scope(vec![a, a]), &[1, 2, 4], true),
        (Command::Stop, covenant_scope(vec![b]), &[1, 2], false),
        (Command::Start, covenant_scope(vec![a]), &[1, 2, 3], true),
        (Command::Start, all(), &[1, 2, 3, 4], false),
        (Command::Stop, all(), &[], false),
        (Command::Start, covenant_scope(vec![a]), &[1, 3], true),
        (Command::Stop, all(), &[], false),
    ];
    for (command, scope, ids, has_covenant_ids) in steps {
        subscription.mutate(Mutation::new(*command, scope.clone()), MutationPolicies::default(), &context).unwrap();
        assert_eq!(single.data().has_covenant_ids(), *has_covenant_ids);
        let filtered = IndexNotification::UtxosChanged(notification.clone()).apply_subscription(&*subscription, &context);
        let rpc_filtered = RpcNotification::UtxosChanged(original.clone()).apply_subscription(&*subscription, &context);
        if ids.is_empty() {
            assert!(filtered.is_none());
            assert!(rpc_filtered.is_none());
        } else {
            let Some(IndexNotification::UtxosChanged(filtered)) = filtered else { panic!("expected index UTXO changes") };
            let Some(RpcNotification::UtxosChanged(rpc_filtered)) = rpc_filtered else { panic!("expected RPC UTXO changes") };
            assert_entries(&UtxosChangedNotification::from(&filtered).added, ids, &original.added);
            assert_entries(&rpc_filtered.added, ids, &original.added);
        }
    }
}

#[test]
fn covenant_notification_filtering_agrees_before_and_after_rpc_conversion() {
    let [a, b, c, d, other] = [101, 102, 103, 104, 105].map(RpcHash::from_u64_word);
    // A script bucket contains different covenants and an ordinary UTXO. Covenant A
    // spans both scripts; neither script represents a standard address.
    let notification = IndexUtxosChanged {
        added: Arc::new(utxo_set(&[
            (1, 1, 20, Some(a)),
            (1, 2, 21, Some(b)),
            (1, 3, 22, None),
            (2, 4, 23, Some(a)),
            (2, 5, 24, Some(c)),
        ])),
        removed: Arc::new(utxo_set(&[
            (1, 1, 10, Some(a)), // Same outpoint re-added above with a new accepting score.
            (1, 6, 11, Some(b)),
            (2, 7, 12, None),
            (2, 8, 13, Some(d)),
        ])),
    };
    let rpc = UtxosChangedNotification::from(&notification);
    assert!(rpc.added.iter().chain(rpc.removed.iter()).all(|entry| entry.address.is_none()));

    let cases: &[(Vec<RpcHash>, &[u64], &[u64])] = &[
        (vec![a], &[1, 4], &[1]),
        (vec![a, b], &[1, 2, 4], &[1, 6]),
        (vec![c], &[5], &[]), // Additions only.
        (vec![d], &[], &[8]), // Removals only.
        (vec![a, b, c, d], &[1, 2, 4, 5], &[1, 6, 8]),
        (vec![other], &[], &[]),
        (vec![], &[], &[]), // Empty filters match nothing, not everything.
    ];
    for (ids, added_ids, removed_ids) in cases {
        let context = SubscriptionContext::new();
        let mut subscription: DynSubscription = Arc::new(UtxosChangedSubscription::new(UtxosChangedState::None, 1));
        if !ids.is_empty() {
            subscription
                .mutate(
                    Mutation::new(Command::Start, UtxosChangedFilteredScope::new(vec![], ids.clone()).into()),
                    MutationPolicies::default(),
                    &context,
                )
                .unwrap();
        }
        let index_filtered = IndexNotification::UtxosChanged(notification.clone()).apply_subscription(&*subscription, &context);
        let rpc_filtered = RpcNotification::UtxosChanged(rpc.clone()).apply_subscription(&*subscription, &context);
        if added_ids.is_empty() && removed_ids.is_empty() {
            assert!(index_filtered.is_none());
            assert!(rpc_filtered.is_none());
            continue;
        }
        let Some(IndexNotification::UtxosChanged(index_filtered)) = index_filtered else { panic!("expected index UTXO changes") };
        let Some(RpcNotification::UtxosChanged(rpc_filtered)) = rpc_filtered else { panic!("expected RPC UTXO changes") };
        let index_as_rpc = UtxosChangedNotification::from(&index_filtered);
        for filtered in [&index_as_rpc, &rpc_filtered] {
            assert_entries(&filtered.added, added_ids, &rpc.added);
            assert_entries(&filtered.removed, removed_ids, &rpc.removed);
        }
    }

    // Filtering leaves the original batch intact, including both sides of a score change.
    let original = UtxosChangedNotification::from(&notification);
    assert_entries(&original.added, &[1, 2, 3, 4, 5], &rpc.added);
    assert_entries(&original.removed, &[1, 6, 7, 8], &rpc.removed);
}

fn utxo_set(rows: &[(u8, u64, u64, Option<RpcHash>)]) -> UtxoSetByScriptPublicKey {
    let mut set = UtxoSetByScriptPublicKey::new();
    for &(script, tx, daa_score, covenant_id) in rows {
        set.entry(ScriptPublicKey::from_vec(0, vec![script])).or_default().insert(
            UtxoEntryKeySuffixRecord::new(daa_score, TransactionOutpoint::new(RpcHash::from_u64_word(tx), 0)),
            CompactUtxoEntry::new(tx * 100, tx == 1, covenant_id),
        );
    }
    set
}

fn assert_entries(actual: &[RpcUtxosByAddressesEntry], ids: &[u64], original: &[RpcUtxosByAddressesEntry]) {
    assert_eq!(actual.len(), ids.len());
    assert_eq!(
        actual.iter().map(|entry| entry.outpoint.transaction_id).collect::<HashSet<_>>(),
        ids.iter().copied().map(RpcHash::from_u64_word).collect::<HashSet<_>>()
    );
    for entry in actual {
        let source = original.iter().find(|source| source.outpoint == entry.outpoint).unwrap();
        assert_eq!(entry.utxo_entry, source.utxo_entry);
        assert_eq!(entry.address, source.address);
    }
}
