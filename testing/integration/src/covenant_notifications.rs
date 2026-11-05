use crate::common::daemon::Daemon;
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionOutpoint};
use kaspa_core::task::runtime::AsyncRuntime;
use kaspa_hashes::Hash;
use kaspa_index_core::{
    indexed_utxos::{CompactUtxoEntry, UtxoEntryKeySuffixRecord, UtxoSetByScriptPublicKey},
    notification::{Notification as IndexNotification, UtxosChangedNotification as IndexUtxosChanged},
};
use kaspa_index_processor::service::IndexService;
use kaspa_notify::{
    connection::{ChannelConnection, ChannelType},
    notifier::Notify,
    scope::{UtxosChangedFilteredScope, UtxosChangedScope},
};
use kaspa_rpc_core::{Notification, UtxosChangedNotification, api::rpc::RpcApi};
use kaspa_txscript::pay_to_address_script;
use kaspa_utils::fd_budget;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding, prelude::ConnectOptions};
use kaspa_wrpc_server::address::WrpcNetAddress;
use kaspad_lib::args::Args;
use std::{collections::HashSet, sync::Arc, time::Duration};

#[tokio::test]
async fn covenant_utxo_notifications_across_transports() {
    kaspa_core::log::try_init_logger("info");
    // Feed index notifications directly: this exercises both subscription/filtering
    // layers and transports without mining covenants or enabling the covenant DB index.
    let mut daemon = Daemon::new_random_with_args(
        Args { simnet: true, disable_upnp: true, utxoindex: true, covenant_index: false, ..Default::default() },
        fd_budget::test_limit(),
    );
    let bootstrap = daemon.start().await;
    let grpc = daemon.new_multi_listener_client().await;
    grpc.start(None).await;
    let borsh = daemon.new_wrpc_client();
    let json_port = match daemon.args.read().rpclisten_json.clone().unwrap() {
        WrpcNetAddress::Custom(address) => address.normalize(0).port,
        _ => unreachable!(),
    };
    let json =
        KaspaRpcClient::new(WrpcEncoding::SerdeJson, Some(&format!("ws://localhost:{json_port}")), None, Some(daemon.network), None)
            .unwrap();
    for client in [&borsh, &json] {
        client.connect(Some(ConnectOptions { block_async_connect: true, ..Default::default() })).await.unwrap();
    }
    let runtime = Arc::downcast::<AsyncRuntime>(daemon.core.find(AsyncRuntime::IDENT).unwrap().into_any_arc()).unwrap();
    let index = Arc::downcast::<IndexService>(runtime.find("index-processor").unwrap().into_any_arc()).unwrap();
    let notifier = index.notifier();
    let id = Hash::from_u64_word(101);
    let address = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
    let mut listeners = vec![];
    for client in [&grpc as &dyn RpcApi, &borsh, &json] {
        let (sender, receiver) = async_channel::unbounded();
        let listener = client.register_new_listener(ChannelConnection::new("covenant test", sender, ChannelType::Closable));
        client.start_notify(listener, UtxosChangedFilteredScope::new(vec![], vec![id, id]).into()).await.unwrap();
        listeners.push((client, listener, receiver));
    }

    for phase in 1..=3 {
        if phase == 2 {
            for (client, listener, _) in &listeners {
                client.start_notify(*listener, UtxosChangedScope::new(vec![address.clone()]).into()).await.unwrap();
            }
        } else if phase == 3 {
            for (client, listener, _) in &listeners {
                client.stop_notify(*listener, UtxosChangedFilteredScope::new(vec![], vec![id]).into()).await.unwrap();
            }
        }
        let batch = notification_batch(&address, id, phase);
        for (client_index, (_, _, receiver)) in listeners.iter().enumerate() {
            // Subscription acknowledgements currently precede upstream propagation.
            // Repeated probes wait for installation; this test does not claim a fence.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    notifier.notify(IndexNotification::UtxosChanged(batch.clone())).unwrap();
                    if let Ok(Ok(Notification::UtxosChanged(notification))) =
                        tokio::time::timeout(Duration::from_millis(30), receiver.recv()).await
                    {
                        let ids: HashSet<_> = notification.added.iter().map(|entry| entry.outpoint.transaction_id).collect();
                        let expected: HashSet<_> = match phase {
                            1 => vec![1, 4],
                            2 => vec![1, 2, 3, 4],
                            3 => vec![1, 2, 3],
                            _ => unreachable!(),
                        }
                        .into_iter()
                        .map(|tx| Hash::from_u64_word(phase * 100 + tx))
                        .collect();
                        if ids == expected {
                            assert_eq!(notification.added.len(), expected.len(), "overlapping filters must not duplicate entries");
                            assert_removed(&notification, phase);
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("phase {phase}, client {client_index}: subscription did not produce the expected covenant/address union")
            });
        }
    }
    for (client, listener, _) in &listeners {
        client.stop_notify(*listener, UtxosChangedScope::default().into()).await.unwrap();
        client.unregister_listener(*listener).await.unwrap();
    }
    borsh.disconnect().await.unwrap();
    json.disconnect().await.unwrap();
    grpc.disconnect().await.unwrap();
    bootstrap.disconnect().await.unwrap();
    daemon.shutdown();
}

fn notification_batch(address: &Address, covenant_id: Hash, phase: u64) -> IndexUtxosChanged {
    let script = pay_to_address_script(address);
    let other_script = ScriptPublicKey::from_vec(0, vec![0x51]);
    let other_id = Hash::from_u64_word(102);
    let mut added = UtxoSetByScriptPublicKey::new();
    for (spk, tx, id) in [
        (script.clone(), 1, Some(covenant_id)),
        (script.clone(), 2, Some(other_id)),
        (script.clone(), 3, None),
        (other_script, 4, Some(covenant_id)),
    ] {
        added.entry(spk).or_default().insert(
            UtxoEntryKeySuffixRecord::new(20, TransactionOutpoint::new(Hash::from_u64_word(phase * 100 + tx), 0)),
            CompactUtxoEntry::new(tx * 100, false, id),
        );
    }
    let mut removed = UtxoSetByScriptPublicKey::new();
    removed.entry(script).or_default().insert(
        UtxoEntryKeySuffixRecord::new(10, TransactionOutpoint::new(Hash::from_u64_word(phase * 100 + 1), 0)),
        CompactUtxoEntry::new(100, false, Some(covenant_id)),
    );
    IndexUtxosChanged { added: Arc::new(added), removed: Arc::new(removed) }
}

fn assert_removed(notification: &UtxosChangedNotification, phase: u64) {
    assert_eq!(notification.removed.len(), 1);
    assert_eq!(notification.removed[0].outpoint.transaction_id, Hash::from_u64_word(phase * 100 + 1));
    assert_eq!(notification.removed[0].utxo_entry.block_daa_score, 10);
    assert_eq!(
        notification
            .added
            .iter()
            .find(|entry| entry.outpoint.transaction_id == Hash::from_u64_word(phase * 100 + 1))
            .unwrap()
            .utxo_entry
            .block_daa_score,
        20
    );
}
