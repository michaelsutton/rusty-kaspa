use kaspa_notify::{scope::Scope, subscription::Command};

use crate::protowire::{
    kaspad_request, kaspad_response, KaspadRequest, KaspadResponse, NotifyBlockAddedRequestMessage,
    NotifyFinalityConflictRequestMessage, NotifyNewBlockTemplateRequestMessage, NotifyPruningPointUtxoSetOverrideRequestMessage,
    NotifySinkBlueScoreChangedRequestMessage, NotifyUtxosChangedRequestMessage, NotifyVirtualChainChangedRequestMessage,
    NotifyVirtualDaaScoreChangedRequestMessage,
};

impl KaspadRequest {
    pub fn from_notification_type(scope: &Scope, command: Command) -> Self {
        KaspadRequest { id: 0, payload: Some(kaspad_request::Payload::from_notification_type(scope, command)) }
    }

    pub fn is_subscription(&self) -> bool {
        self.payload.as_ref().is_some_and(|x| x.is_subscription())
    }
}

impl kaspad_request::Payload {
    pub fn from_notification_type(scope: &Scope, command: Command) -> Self {
        match scope {
            Scope::BlockAdded(_) => {
                kaspad_request::Payload::NotifyBlockAddedRequest(NotifyBlockAddedRequestMessage { command: command.into() })
            }
            Scope::NewBlockTemplate(_) => {
                kaspad_request::Payload::NotifyNewBlockTemplateRequest(NotifyNewBlockTemplateRequestMessage {
                    command: command.into(),
                })
            }

            Scope::VirtualChainChanged(ref scope) => {
                kaspad_request::Payload::NotifyVirtualChainChangedRequest(NotifyVirtualChainChangedRequestMessage {
                    command: command.into(),
                    include_accepted_transaction_ids: scope.include_accepted_transaction_ids,
                })
            }
            Scope::FinalityConflict(_) => {
                kaspad_request::Payload::NotifyFinalityConflictRequest(NotifyFinalityConflictRequestMessage {
                    command: command.into(),
                })
            }
            Scope::FinalityConflictResolved(_) => {
                kaspad_request::Payload::NotifyFinalityConflictRequest(NotifyFinalityConflictRequestMessage {
                    command: command.into(),
                })
            }
            Scope::UtxosChanged(ref scope) => kaspad_request::Payload::NotifyUtxosChangedRequest(NotifyUtxosChangedRequestMessage {
                addresses: scope.addresses.iter().map(|x| x.into()).collect::<Vec<String>>(),
                command: command.into(),
            }),
            Scope::SinkBlueScoreChanged(_) => {
                kaspad_request::Payload::NotifySinkBlueScoreChangedRequest(NotifySinkBlueScoreChangedRequestMessage {
                    command: command.into(),
                })
            }
            Scope::VirtualDaaScoreChanged(_) => {
                kaspad_request::Payload::NotifyVirtualDaaScoreChangedRequest(NotifyVirtualDaaScoreChangedRequestMessage {
                    command: command.into(),
                })
            }
            Scope::PruningPointUtxoSetOverride(_) => {
                kaspad_request::Payload::NotifyPruningPointUtxoSetOverrideRequest(NotifyPruningPointUtxoSetOverrideRequestMessage {
                    command: command.into(),
                })
            }
        }
    }

    pub fn is_subscription(&self) -> bool {
        use crate::protowire::kaspad_request::Payload;
        matches!(
            self,
            Payload::NotifyBlockAddedRequest(_)
                | Payload::NotifyVirtualChainChangedRequest(_)
                | Payload::NotifyFinalityConflictRequest(_)
                | Payload::NotifyUtxosChangedRequest(_)
                | Payload::NotifySinkBlueScoreChangedRequest(_)
                | Payload::NotifyVirtualDaaScoreChangedRequest(_)
                | Payload::NotifyPruningPointUtxoSetOverrideRequest(_)
                | Payload::NotifyNewBlockTemplateRequest(_)
                | Payload::StopNotifyingUtxosChangedRequest(_)
                | Payload::StopNotifyingPruningPointUtxoSetOverrideRequest(_)
        )
    }

    pub fn var_name(&self) -> &str {
        match self {
            kaspad_request::Payload::GetCurrentNetworkRequest(_) => "GetCurrentNetworkRequest",
            kaspad_request::Payload::SubmitBlockRequest(_) => "SubmitBlockRequest",
            kaspad_request::Payload::GetBlockTemplateRequest(_) => "GetBlockTemplateRequest",
            kaspad_request::Payload::NotifyBlockAddedRequest(_) => "NotifyBlockAddedRequest",
            kaspad_request::Payload::GetPeerAddressesRequest(_) => "GetPeerAddressesRequest",
            kaspad_request::Payload::GetSelectedTipHashRequest(_) => "GetSelectedTipHashRequest",
            kaspad_request::Payload::GetMempoolEntryRequest(_) => "GetMempoolEntryRequest",
            kaspad_request::Payload::GetConnectedPeerInfoRequest(_) => "GetConnectedPeerInfoRequest",
            kaspad_request::Payload::AddPeerRequest(_) => "AddPeerRequest",
            kaspad_request::Payload::SubmitTransactionRequest(_) => "SubmitTransactionRequest",
            kaspad_request::Payload::NotifyVirtualChainChangedRequest(_) => "NotifyVirtualChainChangedRequest",
            kaspad_request::Payload::GetBlockRequest(_) => "GetBlockRequest",
            kaspad_request::Payload::GetSubnetworkRequest(_) => "GetSubnetworkRequest",
            kaspad_request::Payload::GetVirtualChainFromBlockRequest(_) => "GetVirtualChainFromBlockRequest",
            kaspad_request::Payload::GetBlocksRequest(_) => "GetBlocksRequest",
            kaspad_request::Payload::GetBlockCountRequest(_) => "GetBlockCountRequest",
            kaspad_request::Payload::GetBlockDagInfoRequest(_) => "GetBlockDagInfoRequest",
            kaspad_request::Payload::ResolveFinalityConflictRequest(_) => "ResolveFinalityConflictRequest",
            kaspad_request::Payload::NotifyFinalityConflictRequest(_) => "NotifyFinalityConflictRequest",
            kaspad_request::Payload::GetMempoolEntriesRequest(_) => "GetMempoolEntriesRequest",
            kaspad_request::Payload::ShutdownRequest(_) => "ShutdownRequest",
            kaspad_request::Payload::GetHeadersRequest(_) => "GetHeadersRequest",
            kaspad_request::Payload::NotifyUtxosChangedRequest(_) => "NotifyUtxosChangedRequest",
            kaspad_request::Payload::GetUtxosByAddressesRequest(_) => "GetUtxosByAddressesRequest",
            kaspad_request::Payload::GetSinkBlueScoreRequest(_) => "GetSinkBlueScoreRequest",
            kaspad_request::Payload::NotifySinkBlueScoreChangedRequest(_) => "NotifySinkBlueScoreChangedRequest",
            kaspad_request::Payload::BanRequest(_) => "BanRequest",
            kaspad_request::Payload::UnbanRequest(_) => "UnbanRequest",
            kaspad_request::Payload::GetInfoRequest(_) => "GetInfoRequest",
            kaspad_request::Payload::StopNotifyingUtxosChangedRequest(_) => "StopNotifyingUtxosChangedRequest",
            kaspad_request::Payload::NotifyPruningPointUtxoSetOverrideRequest(_) => "NotifyPruningPointUtxoSetOverrideRequest",
            kaspad_request::Payload::StopNotifyingPruningPointUtxoSetOverrideRequest(_) => {
                "StopNotifyingPruningPointUtxoSetOverrideRequest"
            }
            kaspad_request::Payload::EstimateNetworkHashesPerSecondRequest(_) => "EstimateNetworkHashesPerSecondRequest",
            kaspad_request::Payload::NotifyVirtualDaaScoreChangedRequest(_) => "NotifyVirtualDaaScoreChangedRequest",
            kaspad_request::Payload::GetBalanceByAddressRequest(_) => "GetBalanceByAddressRequest",
            kaspad_request::Payload::GetBalancesByAddressesRequest(_) => "GetBalancesByAddressesRequest",
            kaspad_request::Payload::NotifyNewBlockTemplateRequest(_) => "NotifyNewBlockTemplateRequest",
            kaspad_request::Payload::GetMempoolEntriesByAddressesRequest(_) => "GetMempoolEntriesByAddressesRequest",
            kaspad_request::Payload::GetCoinSupplyRequest(_) => "GetCoinSupplyRequest",
            kaspad_request::Payload::PingRequest(_) => "PingRequest",
            kaspad_request::Payload::GetProcessMetricsRequest(_) => "GetProcessMetricsRequest",
        }
    }
}

impl KaspadResponse {
    pub fn is_notification(&self) -> bool {
        match self.payload {
            Some(ref payload) => payload.is_notification(),
            None => false,
        }
    }
}

#[allow(clippy::match_like_matches_macro)]
impl kaspad_response::Payload {
    pub fn is_notification(&self) -> bool {
        use crate::protowire::kaspad_response::Payload;
        match self {
            Payload::BlockAddedNotification(_) => true,
            Payload::VirtualChainChangedNotification(_) => true,
            Payload::FinalityConflictNotification(_) => true,
            Payload::FinalityConflictResolvedNotification(_) => true,
            Payload::UtxosChangedNotification(_) => true,
            Payload::SinkBlueScoreChangedNotification(_) => true,
            Payload::VirtualDaaScoreChangedNotification(_) => true,
            Payload::PruningPointUtxoSetOverrideNotification(_) => true,
            Payload::NewBlockTemplateNotification(_) => true,
            _ => false,
        }
    }
}
