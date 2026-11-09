use super::events::EventType;
use crate::address::tracker::TrackerKey;
use borsh::{BorshDeserialize, BorshSerialize};
use derive_more::Display;
use kaspa_addresses::{Address, Prefix};
use kaspa_hashes::Hash;
use kaspa_txscript::extract_script_pub_key_address;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use workflow_serializer::prelude::*;

macro_rules! scope_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
    $($(#[$variant_meta:meta])* $variant_name:ident,)*
    } aliases { $($alias:ident => $event:ident,)* }) => {
        paste::paste! {
            $(#[$meta])*
            $vis enum $name {
                $($(#[$variant_meta])* $variant_name([<$variant_name Scope>])),*,
                $($alias([<$alias Scope>])),*
            }

            impl From<&$name> for EventType {
                fn from(value: &$name) -> Self {
                    match value {
                        $($name::$variant_name(_) => EventType::$variant_name,)*
                        $($name::$alias(_) => EventType::$event,)*
                    }
                }
            }

            impl std::convert::From<EventType> for $name {
                fn from(value: EventType) -> Self {
                    match value {
                        $(EventType::$variant_name => $name::$variant_name(kaspa_notify::scope::[<$variant_name Scope>]::default())),*
                    }
                }
            }

            $(impl std::convert::From<[<$variant_name Scope>]> for Scope {
                fn from(value: [<$variant_name Scope>]) -> Self {
                    Scope::$variant_name(value)
                }
            })*
            $(impl From<[<$alias Scope>]> for Scope {
                fn from(value: [<$alias Scope>]) -> Self {
                    Scope::$alias(value)
                }
            })*
        }
    }
}

scope_enum! {
/// Subscription scope for every event type
#[derive(Clone, Display, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum Scope {
    BlockAdded,
    VirtualChainChanged,
    FinalityConflict,
    FinalityConflictResolved,
    UtxosChanged,
    SinkBlueScoreChanged,
    VirtualDaaScoreChanged,
    PruningPointUtxoSetOverride,
    NewBlockTemplate,
}
aliases {
    UtxosChangedFiltered => UtxosChanged,
}
}

impl Scope {
    pub fn event_type(&self) -> EventType {
        self.into()
    }

    pub(crate) fn into_utxos_changed_filters(self) -> Option<(UtxosChangedFilteredScope, bool)> {
        match self {
            Self::UtxosChanged(scope) => {
                let blanket = scope.addresses.is_empty();
                Some((UtxosChangedFilteredScope::new(scope.addresses, vec![]), blanket))
            }
            Self::UtxosChangedFiltered(scope) => Some((scope, false)),
            _ => None,
        }
    }
}

impl Serializer for Scope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        store!(Scope, self, writer)?;
        Ok(())
    }
}

impl Deserializer for Scope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        load!(Scope, reader)
    }
}

#[derive(Clone, Display, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct BlockAddedScope {}

impl Serializer for BlockAddedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for BlockAddedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct VirtualChainChangedScope {
    pub include_accepted_transaction_ids: bool,
}

impl VirtualChainChangedScope {
    pub fn new(include_accepted_transaction_ids: bool) -> Self {
        Self { include_accepted_transaction_ids }
    }
}

impl std::fmt::Display for VirtualChainChangedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VirtualChainChangedScope{}", if self.include_accepted_transaction_ids { " with accepted transactions" } else { "" })
    }
}

impl Serializer for VirtualChainChangedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        store!(bool, &self.include_accepted_transaction_ids, writer)?;
        Ok(())
    }
}

impl Deserializer for VirtualChainChangedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        let include_accepted_transaction_ids = load!(bool, reader)?;
        Ok(Self { include_accepted_transaction_ids })
    }
}

#[derive(Clone, Display, Debug, PartialEq, Eq, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct FinalityConflictScope {}

impl Serializer for FinalityConflictScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for FinalityConflictScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

#[derive(Clone, Display, Debug, PartialEq, Eq, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct FinalityConflictResolvedScope {}

impl Serializer for FinalityConflictResolvedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for FinalityConflictResolvedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

/// Address-filtered or blanket scope for [`EventType::UtxosChanged`].
///
/// This scope remains supported with its existing wire encoding. A nonempty
/// address list selects matching UTXOs. An empty list represents a blanket scope:
/// `Start` subscribes to all UTXO changes, and `Stop` clears all address and covenant
/// filters. Internal blanket subscriptions and mutations continue to use this form.
#[derive(Clone, Debug, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct UtxosChangedScope {
    pub addresses: Vec<Address>,
}

impl std::fmt::Display for UtxosChangedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let addresses = match self.addresses.len() {
            0 => "all".to_string(),
            1 => format!("{}", self.addresses[0]),
            n => format!("{} addresses", n),
        };
        write!(f, "UtxosChangedScope ({})", addresses)
    }
}

impl PartialEq for UtxosChangedScope {
    fn eq(&self, other: &Self) -> bool {
        self.addresses.len() == other.addresses.len() && self.addresses.iter().all(|x| other.addresses.contains(x))
    }
}

impl Eq for UtxosChangedScope {}

impl UtxosChangedScope {
    pub fn new(addresses: Vec<Address>) -> Self {
        Self { addresses }
    }
}

/// Explicit address/covenant-ID filters for [`EventType::UtxosChanged`].
///
/// Address and covenant matches are combined with OR. At least one list must be
/// nonempty; blanket subscriptions use [`UtxosChangedScope`] with no addresses.
/// Both scope forms represent the same event. The separate wire representation
/// allows older nodes to reject unsupported filters instead of interpreting a
/// covenant-only subscription as a blanket subscription.
#[derive(Clone, Debug, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtxosChangedFilteredScope {
    pub addresses: Vec<Address>,
    pub covenant_ids: Vec<Hash>,
}

impl PartialEq for UtxosChangedFilteredScope {
    fn eq(&self, other: &Self) -> bool {
        self.addresses.iter().collect::<HashSet<_>>() == other.addresses.iter().collect::<HashSet<_>>()
            && self.covenant_ids.iter().collect::<HashSet<_>>() == other.covenant_ids.iter().collect::<HashSet<_>>()
    }
}

impl Eq for UtxosChangedFilteredScope {}

impl UtxosChangedFilteredScope {
    pub fn new(addresses: Vec<Address>, covenant_ids: Vec<Hash>) -> Self {
        Self { addresses, covenant_ids }
    }

    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty() && self.covenant_ids.is_empty()
    }

    pub(crate) fn into_keys(self) -> Vec<TrackerKey> {
        self.addresses.into_iter().map(TrackerKey::from).chain(self.covenant_ids.into_iter().map(TrackerKey::Cov)).collect()
    }

    pub(crate) fn from_keys<'a>(keys: impl IntoIterator<Item = &'a TrackerKey>, prefix: Prefix) -> Self {
        let mut filters = Self::default();
        for key in keys {
            filters.push_key(key, prefix);
        }
        filters
    }

    pub(crate) fn push_key(&mut self, key: &TrackerKey, prefix: Prefix) {
        match key {
            TrackerKey::Spk(spk) => self
                .addresses
                .push(extract_script_pub_key_address(spk, prefix).expect("SPK subscription keys originate from addresses")),
            TrackerKey::Cov(id) => self.covenant_ids.push(*id),
        }
    }

    pub(crate) fn into_scope(self) -> Scope {
        if self.covenant_ids.is_empty() { UtxosChangedScope::new(self.addresses).into() } else { self.into() }
    }
}

impl std::fmt::Display for UtxosChangedFilteredScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UtxosChangedFilteredScope ({} addresses, {} covenant IDs)", self.addresses.len(), self.covenant_ids.len())
    }
}

impl Serializer for UtxosChangedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        store!(Vec<Address>, &self.addresses, writer)?;
        Ok(())
    }
}

impl Deserializer for UtxosChangedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        let addresses = load!(Vec<Address>, reader)?;
        Ok(Self { addresses })
    }
}

#[derive(Clone, Display, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct SinkBlueScoreChangedScope {}

impl Serializer for SinkBlueScoreChangedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for SinkBlueScoreChangedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

#[derive(Clone, Display, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct VirtualDaaScoreChangedScope {}

impl Serializer for VirtualDaaScoreChangedScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for VirtualDaaScoreChangedScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

#[derive(Clone, Display, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct PruningPointUtxoSetOverrideScope {}

impl Serializer for PruningPointUtxoSetOverrideScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for PruningPointUtxoSetOverrideScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}

#[derive(Clone, Display, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NewBlockTemplateScope {}

impl Serializer for NewBlockTemplateScope {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        store!(u16, &1, writer)?;
        Ok(())
    }
}

impl Deserializer for NewBlockTemplateScope {
    fn deserialize<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let _version = load!(u16, reader)?;
        Ok(Self {})
    }
}
