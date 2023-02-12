use crate::{pb::KaspadMessage, ConnectionInitializer, Router};
use kaspa_core::debug;
use parking_lot::RwLock;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc::Receiver as MpscReceiver;
use uuid::Uuid;

#[derive(Debug)]
pub(crate) enum HubEvent {
    NewPeer(Arc<Router>),
    PeerClosing(Uuid),
    Broadcast(Box<KaspadMessage>),
}

#[derive(Debug, Clone)]
pub(crate) struct Hub {
    /// Map of currently active peers
    pub(crate) active_peers: Arc<RwLock<HashMap<Uuid, Arc<Router>>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self { active_peers: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Starts a loop for receiving central hub events from all peer routers. This mechanism is used for
    /// managing a collection of active peers and for supporting a broadcast operation.
    pub(crate) fn start_event_loop(self, mut hub_receiver: MpscReceiver<HubEvent>, initializer: Arc<dyn ConnectionInitializer>) {
        tokio::spawn(async move {
            while let Some(new_event) = hub_receiver.recv().await {
                match new_event {
                    HubEvent::NewPeer(new_router) => {
                        match initializer.initialize_connection(new_router.clone()).await {
                            Ok(_) => {
                                self.active_peers.write().insert(new_router.identity(), new_router);
                            }
                            Err(err) => {
                                // Ignoring the router
                                new_router.close().await;
                                debug!("P2P, flow initialization for router-id {:?} failed: {}", new_router.identity(), err);
                            }
                        }
                    }
                    HubEvent::PeerClosing(peer_id) => {
                        if let Some(router) = self.active_peers.write().remove(&peer_id) {
                            debug!(
                                "P2P, Hub event loop, removing peer, router-id: {}, {}",
                                router.identity(),
                                Arc::strong_count(&router)
                            );
                        }
                    }
                    HubEvent::Broadcast(msg) => {
                        self.broadcast(*msg).await;
                    }
                }
            }
            debug!("P2P, Hub event loop exiting");
        });
    }

    /// Send a message to a specific peer
    pub async fn send(&self, peer_id: Uuid, msg: KaspadMessage) -> bool {
        let op = self.active_peers.read().get(&peer_id).cloned();
        if let Some(router) = op {
            router.route_to_network(msg).await
        } else {
            false
        }
    }

    /// Broadcast a message to all peers. Note that broadcast can also be called on a specific router and will lead to the same outcome
    pub async fn broadcast(&self, msg: KaspadMessage) {
        let peers = self.active_peers.read().values().cloned().collect::<Vec<_>>();
        for router in peers {
            router.route_to_network(msg.clone()).await;
        }
    }

    /// Terminate a specific peer
    pub async fn terminate(&self, peer_id: Uuid) {
        let op = self.active_peers.read().get(&peer_id).cloned();
        if let Some(router) = op {
            // This will eventually lead to peer removal through the Hub event loop
            router.close().await;
        }
    }

    /// Terminate all peers
    pub async fn terminate_all_peers(&self) {
        let peers = self.active_peers.write().drain().map(|(_, r)| r).collect::<Vec<_>>();
        for router in peers {
            router.close().await;
        }
    }

    /// Returns a list of ids for all currently active peers
    pub async fn get_active_peers(&self) -> Vec<Uuid> {
        self.active_peers.read().keys().cloned().collect()
    }
}