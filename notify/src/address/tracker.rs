use crate::address::error::{Error, Result};
use indexmap::{Equivalent, IndexMap, map::Entry};
use itertools::Itertools;
use kaspa_addresses::{Address, Prefix};
use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_core::{debug, trace};
use kaspa_hashes::Hash as CovenantId;
use kaspa_txscript::{extract_script_pub_key_address, pay_to_address_script};
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::{
    collections::{HashMap, HashSet, hash_map, hash_set},
    fmt::Display,
};

/// A subscription target interned in the shared tracker index space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackerKey {
    Spk(ScriptPublicKey),
    Cov(CovenantId),
}

/// Borrowed lookup key, avoiding script allocations during notification filtering.
#[derive(Clone, Copy, Hash)]
pub enum TrackerKeyRef<'a> {
    Spk(&'a ScriptPublicKey),
    Cov(&'a CovenantId),
}

impl TrackerKey {
    pub fn as_key_ref(&self) -> TrackerKeyRef<'_> {
        match self {
            Self::Spk(spk) => TrackerKeyRef::Spk(spk),
            Self::Cov(id) => TrackerKeyRef::Cov(id),
        }
    }
}

impl std::hash::Hash for TrackerKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.as_key_ref(), state);
    }
}

impl Equivalent<TrackerKey> for TrackerKeyRef<'_> {
    fn equivalent(&self, key: &TrackerKey) -> bool {
        match (self, key) {
            (Self::Spk(a), TrackerKey::Spk(b)) => *a == b,
            (Self::Cov(a), TrackerKey::Cov(b)) => *a == b,
            _ => false,
        }
    }
}

impl From<Address> for TrackerKey {
    fn from(address: Address) -> Self {
        Self::Spk(pay_to_address_script(&address))
    }
}

impl From<ScriptPublicKey> for TrackerKey {
    fn from(spk: ScriptPublicKey) -> Self {
        Self::Spk(spk)
    }
}

impl<'a> From<&'a ScriptPublicKey> for TrackerKeyRef<'a> {
    fn from(spk: &'a ScriptPublicKey) -> Self {
        Self::Spk(spk)
    }
}

impl<'a> From<&'a CovenantId> for TrackerKeyRef<'a> {
    fn from(id: &'a CovenantId) -> Self {
        Self::Cov(id)
    }
}

pub trait Indexer {
    fn contains(&self, index: Index) -> bool;

    /// Inserts an [`Index`].
    ///
    /// Returns true if the index was not present and was successfully inserted, false otherwise.
    fn insert(&mut self, index: Index) -> bool;

    /// Returns true when inserting an already-present index mutates the indexer.
    fn insertion_mutates_existing(&self) -> bool {
        false
    }

    /// Removes an [`Index`].
    ///
    /// Returns true if the index was present and successfully removed, false otherwise.
    fn remove(&mut self, index: Index) -> bool;

    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
}

pub type Index = u32;
pub type RefCount = u64;

/// Tracks reference count of indexes
pub type Counters = CounterMap;

/// Tracks indexes
pub type Indexes = IndexSet;

/// Tracks reference count of indexes
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CounterMap(HashMap<Index, RefCount>);

impl CounterMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashMap::with_capacity(capacity))
    }

    #[cfg(test)]
    pub fn with_counters(counters: Vec<Counter>) -> Self {
        Self(counters.into_iter().map(|x| (x.index, x.count)).collect())
    }

    pub fn iter(&self) -> hash_map::Iter<'_, Index, RefCount> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl Indexer for CounterMap {
    fn contains(&self, index: Index) -> bool {
        self.0.contains_key(&index)
    }

    fn insert(&mut self, index: Index) -> bool {
        let mut result = true;
        self.0
            .entry(index)
            .and_modify(|x| {
                #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(COUNTER)")]
                {
                    *x += 1;
                }
                result = *x == 1;
            })
            .or_insert(1);
        result
    }

    fn insertion_mutates_existing(&self) -> bool {
        true
    }

    fn remove(&mut self, index: Index) -> bool {
        let mut result = false;
        self.0.entry(index).and_modify(|x| {
            let Some(new_x) = x.checked_sub(1) else {
                return;
            };
            *x = new_x;
            result = *x == 0
        });
        result
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct Counter {
    pub index: Index,
    pub count: RefCount,
    pub locked: bool,
}

#[cfg(test)]
impl Counter {
    pub fn new(index: Index, count: RefCount) -> Self {
        Self { index, count, locked: false }
    }

    pub fn active(&self) -> bool {
        self.count > 0
    }
}

#[cfg(test)]
impl PartialEq for Counter {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}
#[cfg(test)]
impl Eq for Counter {}

#[cfg(test)]
impl PartialOrd for Counter {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[cfg(test)]
impl Ord for Counter {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index.cmp(&other.index)
    }
}

/// Set of `Index`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSet(HashSet<Index>);

impl IndexSet {
    pub fn new(indexes: Vec<Index>) -> Self {
        Self(indexes.into_iter().collect())
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashSet::with_capacity(capacity))
    }

    pub fn iter(&self) -> hash_set::Iter<'_, Index> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    pub fn drain(&mut self) -> hash_set::Drain<'_, Index> {
        self.0.drain()
    }
}

impl Indexer for IndexSet {
    fn contains(&self, index: Index) -> bool {
        self.0.contains(&index)
    }

    fn insert(&mut self, index: Index) -> bool {
        self.0.insert(index)
    }

    fn remove(&mut self, index: Index) -> bool {
        self.0.remove(&index)
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

#[derive(Debug)]
struct Inner {
    /// Index-based map of [`TrackerKey`] to its reference count
    ///
    /// ### Implementation note
    ///
    /// The tracker reduces each SPK or covenant ID to an [`Index`] in all
    /// [`Indexer`] instances. Therefore, every mutable access to the struct must be careful not to
    /// use `IndexMap` APIs which alter the index order of existing entries.
    keys: IndexMap<TrackerKey, RefCount>,

    /// Maximum combined SPK/covenant-ID count that can be registered. Note this must be `<= Index::MAX` since we cast the returned indexes to `Index`
    max_addresses: usize,

    /// Shared key preallocation, configured by the legacy address-capacity option.
    addresses_preallocation: Option<usize>,

    /// Set of entries [`Index`] in `keys` having their [`RefCount`] at 0 hence considered
    /// empty.
    ///
    /// An empty entry can be recycled for either key variant.
    empty_entries: HashSet<Index>,
}

/// Fails at compile time if `MAX_ADDRESS_UPPER_BOUND > Index::MAX`.
/// This is mandatory since we cast the returned indexes to `Index`
const _: usize = Index::MAX as usize - Inner::MAX_ADDRESS_UPPER_BOUND;

impl Inner {
    /// The upper bound of the maximum address count. Note that the upper bound must
    /// never exceed `Index::MAX` since we cast the returned indexes to `Index`. See
    /// compile-time assertion above
    const MAX_ADDRESS_UPPER_BOUND: usize = Self::expand_max_addresses(10_000_000);

    /// The lower bound of the maximum address count
    const MAX_ADDRESS_LOWER_BOUND: usize = 6;

    /// Expanded count for a maximum of 1M addresses
    const DEFAULT_MAX_ADDRESSES: usize = Self::expand_max_addresses(1_000_000);

    /// Computes the optimal expanded max address count fitting in the actual allocated size of
    /// the internal storage structure
    const fn expand_max_addresses(max_addresses: usize) -> usize {
        if max_addresses >= Self::MAX_ADDRESS_LOWER_BOUND {
            // The following formula matches the internal allocation of an IndexMap or a HashMap
            // as found in fns hashbrown::raw::inner::{capacity_to_buckets, bucket_mask_to_capacity}.
            //
            // The last allocated entry is reserved for recycling entries, hence the plus and minus 1
            // which differ from the hashbrown formula.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "expand_max_addresses is only called on initialization, so we don't mind overflow panics."
            )]
            {
                ((max_addresses + 1) * 8 / 7).next_power_of_two() * 7 / 8 - 1
            }
        } else {
            Self::MAX_ADDRESS_LOWER_BOUND
        }
    }

    fn new(max_addresses: Option<usize>) -> Self {
        // Expands the maximum address count to the IndexMap actual usable allocated size minus 1.
        // Saving one entry for the insert/swap_remove scheme during entry recycling prevents a reallocation
        // when reaching the maximum.
        let max_addresses = max_addresses.map(Self::expand_max_addresses);
        let addresses_preallocation = max_addresses;
        let capacity = max_addresses.map(|x| x.saturating_add(1)).unwrap_or_default();

        assert!(
            capacity <= Self::MAX_ADDRESS_UPPER_BOUND + 1,
            "Tracker maximum address count cannot exceed {}",
            Self::MAX_ADDRESS_UPPER_BOUND
        );
        let max_addresses = max_addresses.unwrap_or(Self::DEFAULT_MAX_ADDRESSES);
        debug!("Memory configuration: UTXO changed events will track at most {} SPK/covenant keys", max_addresses);

        let keys = IndexMap::with_capacity(capacity);
        debug!("Creating a subscription key tracker with a capacity of {}", keys.capacity());

        let empty_entries = HashSet::with_capacity(capacity);
        Self { keys, max_addresses, addresses_preallocation, empty_entries }
    }

    fn is_full(&self) -> bool {
        self.keys.len() >= self.max_addresses && self.empty_entries.is_empty()
    }

    fn get(&self, key: TrackerKeyRef<'_>) -> Option<(Index, RefCount)> {
        self.keys.get_full(&key).map(|(index, _, count)| (index as Index, *count))
    }

    fn get_index(&self, index: Index) -> Option<&TrackerKey> {
        self.keys.get_index(index as usize).map(|(key, _)| key)
    }

    fn get_index_address(&self, index: Index, prefix: Prefix) -> Option<Address> {
        match self.get_index(index)? {
            TrackerKey::Spk(spk) => Some(extract_script_pub_key_address(spk, prefix).expect("is retro-convertible")),
            TrackerKey::Cov(_) => None,
        }
    }

    fn get_or_insert(&mut self, key: TrackerKey) -> Result<Index> {
        match self.is_full() {
            false => match self.keys.entry(key) {
                Entry::Occupied(entry) => Ok(entry.index() as Index),
                Entry::Vacant(entry) => {
                    let mut index = entry.index() as Index;
                    trace!("UtxoTracker insert #{} {:?}", index, entry.key());
                    let _ = *entry.insert(0);

                    // Try to recycle an empty entry if there is some
                    let mut recycled = false;
                    if index.checked_add(1).ok_or(Error::MaxCapacityReached)? as usize == self.keys.len()
                        && !self.empty_entries.is_empty()
                    {
                        // Takes the first empty entry index
                        let empty_index = self.empty_entries.iter().cloned().next();
                        if let Some(empty_index) = empty_index {
                            // Stores the newly created entry at the empty entry index while keeping it registered as an
                            // empty entry (because it is so at this stage, the ref count being 0).
                            self.keys.swap_remove_index(empty_index as usize);
                            index = empty_index;
                            recycled = true;
                        }
                    }
                    // If no recycling occurred, registers the newly created entry as empty (since ref count is 0).
                    if !recycled {
                        self.empty_entries.insert(index);
                    }
                    Ok(index)
                }
            },
            true => match self.keys.get_index_of(&key) {
                Some(index) => Ok(index as Index),
                None => Err(Error::MaxCapacityReached),
            },
        }
    }

    /// Increases by one the [`RefCount`] of the key at `index`.
    ///
    /// If the entry had a reference count of 0 before the increase, its index is removed from
    /// the empty entries set.
    fn inc_count(&mut self, index: Index) {
        if let Some((_, count)) = self.keys.get_index_mut(index as usize) {
            #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(COUNTER)")]
            {
                *count += 1;
            }
            trace!("UtxoTracker inc count #{} to {}", index, *count);
            if *count == 1 {
                self.empty_entries.remove(&index);
            }
        }
    }

    /// Decreases by one the [`RefCount`] of the key at `index`.
    ///
    /// Panics if the ref count is already 0.
    ///
    /// When the reference count reaches zero, the index is inserted into the empty entries set.
    fn dec_count(&mut self, index: Index) {
        if let Some((_, count)) = self.keys.get_index_mut(index as usize) {
            *count = count.checked_sub(1).expect("We expect `count > 0`");
            trace!("UtxoTracker dec count #{} to {}", index, *count);
            if *count == 0 {
                self.empty_entries.insert(index);
            }
        }
    }

    fn len(&self) -> usize {
        assert!(self.keys.len() >= self.empty_entries.len(), "entries marked empty are never removed from keys");
        #[allow(clippy::arithmetic_side_effects, reason = "`self.empty_entries.len() <= self.keys.len()`.")]
        {
            self.keys.len() - self.empty_entries.len()
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Shared tracker for SPK and covenant-ID subscription targets.
///
/// Addresses are normalized to SPKs, removing network-prefix duplication. Both
/// key variants share the same bounded index space and registration lifecycle.
/// Address conversion helpers ignore covenant keys and require a network prefix.
#[derive(Debug)]
pub struct Tracker {
    inner: RwLock<Inner>,
}

impl Display for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} subscription keys", self.inner.read().keys.len())
    }
}

impl Tracker {
    /// The upper bound of the maximum address count
    pub const MAX_ADDRESS_UPPER_BOUND: usize = Inner::MAX_ADDRESS_UPPER_BOUND;

    /// Expanded count for a maximum of 1M addresses
    pub const DEFAULT_MAX_ADDRESSES: usize = Inner::DEFAULT_MAX_ADDRESSES;

    const KEY_CHUNK_SIZE: usize = 1024;

    /// Computes the optimal expanded max address count fitting in the actual allocated size of
    /// the internal storage structure
    pub const fn expand_max_addresses(max_addresses: usize) -> usize {
        Inner::expand_max_addresses(max_addresses)
    }

    /// Creates a new `Tracker` instance. If `max_addresses` is `Some`, uses it to prealloc
    /// the internal index as well as for bounding the index size. Otherwise, performs no
    /// prealloc while bounding the index size by `Tracker::DEFAULT_MAX_ADDRESSES`
    pub fn new(max_addresses: Option<usize>) -> Self {
        Self { inner: RwLock::new(Inner::new(max_addresses)) }
    }

    #[cfg(test)]
    pub fn with_addresses(addresses: &[Address]) -> Self {
        let tracker = Self { inner: RwLock::new(Inner::new(None)) };
        for chunk in addresses.chunks(Self::KEY_CHUNK_SIZE) {
            let mut inner = tracker.inner.write();
            for address in chunk {
                let index = inner.get_or_insert(pay_to_address_script(address).into()).unwrap();
                inner.inc_count(index);
            }
        }
        tracker
    }

    pub fn data(&self) -> TrackerReadGuard<'_> {
        TrackerReadGuard { guard: self.inner.read() }
    }

    pub fn get<'a>(&self, key: impl Into<TrackerKeyRef<'a>>) -> Option<(Index, RefCount)> {
        self.inner.read().get(key.into())
    }

    pub fn get_address(&self, address: &Address) -> Option<(Index, RefCount)> {
        self.get(&pay_to_address_script(address))
    }

    pub fn get_address_at_index(&self, index: Index, prefix: Prefix) -> Option<Address> {
        self.inner.read().get_index_address(index, prefix)
    }

    pub fn contains<'a, T: Indexer>(&self, indexes: &T, key: impl Into<TrackerKeyRef<'a>>) -> bool {
        self.get(key).is_some_and(|(index, _)| indexes.contains(index))
    }

    pub fn matches<T: Indexer>(&self, indexes: &T, spk: &ScriptPublicKey, covenant_id: Option<CovenantId>) -> bool {
        let inner = self.inner.read();
        inner.get(TrackerKeyRef::Spk(spk)).is_some_and(|(index, _)| indexes.contains(index))
            || covenant_id.is_some_and(|id| inner.get(TrackerKeyRef::Cov(&id)).is_some_and(|(index, _)| indexes.contains(index)))
    }

    pub fn contains_address<T: Indexer>(&self, indexes: &T, address: &Address) -> bool {
        self.contains(indexes, &pay_to_address_script(address))
    }

    /// Returns an index set containing the indexes of all the addresses both registered in the tracker and in `indexes`.
    pub fn unregistering_indexes(&self, indexes: &Indexes, addresses: &[Address]) -> Indexes {
        Indexes::new(
            addresses
                .iter()
                .filter_map(|address| {
                    self.get(&pay_to_address_script(address)).and_then(|(index, _)| indexes.contains(index).then_some(index))
                })
                .collect(),
        )
    }

    /// Registers targets in the shared key space and the supplied indexer.
    /// Returns the targets whose registration became active in the indexer.
    /// Capacity failures roll back all registrations from this call, across both key variants.
    pub fn register<T: Indexer, K: Clone + Into<TrackerKey>>(&self, indexes: &mut T, mut targets: Vec<K>) -> Result<Vec<K>> {
        let mut rollback = false;
        let mut rollback_targets = vec![];
        {
            let mut counter: usize = 0;
            let mut inner = self.inner.write();
            targets.retain(|target| {
                if rollback {
                    return false;
                }

                #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(INDEX)")]
                {
                    counter += 1;
                }
                if counter.is_multiple_of(Self::KEY_CHUNK_SIZE) {
                    RwLockWriteGuard::bump(&mut inner);
                }
                let key = target.clone().into();

                match inner.get_or_insert(key) {
                    Ok(index) => {
                        // Some indexers mutate existing entries even though `insert` returns false. Since `retain` excludes such
                        // targets from the success result, record them separately so a later failure can still roll them back.
                        let inserted = indexes.insert(index);
                        if !inserted && indexes.insertion_mutates_existing() {
                            rollback_targets.push(target.clone());
                        }
                        if inserted {
                            inner.inc_count(index);
                            true
                        } else {
                            false
                        }
                    }
                    Err(Error::MaxCapacityReached) => {
                        // Rollback registration
                        rollback = true;
                        false
                    }
                }
            });
        }
        match rollback {
            false => Ok(targets),
            true => {
                targets.extend(rollback_targets);
                let _ = self.unregister(indexes, targets);
                Err(Error::MaxCapacityReached)
            }
        }
    }

    /// Unregisters targets from the indexer, releasing shared references on deactivation.
    /// Returns the targets whose registration became inactive in the indexer.
    pub fn unregister<T: Indexer, K: Clone + Into<TrackerKey>>(&self, indexes: &mut T, mut targets: Vec<K>) -> Vec<K> {
        if indexes.is_empty() {
            vec![]
        } else {
            let mut counter: usize = 0;
            let mut inner = self.inner.write();
            targets.retain(|target| {
                #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(INDEX)")]
                {
                    counter += 1;
                }

                if counter.is_multiple_of(Self::KEY_CHUNK_SIZE) {
                    RwLockWriteGuard::bump(&mut inner);
                }
                let key: TrackerKey = target.clone().into();
                if let Some((index, _)) = inner.get(key.as_key_ref()) {
                    if indexes.remove(index) {
                        inner.dec_count(index);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            });
            targets
        }
    }

    /// Unregisters all indexes contained in `indexes`, draining it in the process.
    pub fn unregister_indexes(&self, indexes: &mut Indexes) {
        for chunk in &indexes.drain().chunks(Self::KEY_CHUNK_SIZE) {
            let mut inner = self.inner.write();
            chunk.for_each(|index| inner.dec_count(index));
        }
    }

    pub fn to_addresses(&self, indexes: &[Index], prefix: Prefix) -> Vec<Address> {
        let mut addresses = Vec::with_capacity(indexes.len());
        for chunk in indexes.chunks(Self::KEY_CHUNK_SIZE) {
            let inner = self.inner.read();
            chunk.iter().for_each(|index| {
                if let Some(address) = inner.get_index_address(*index, prefix) {
                    addresses.push(address);
                }
            });
        }
        addresses
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.inner.read().keys.capacity()
    }

    pub fn addresses_preallocation(&self) -> Option<usize> {
        self.inner.read().addresses_preallocation
    }
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(None)
    }
}

pub struct TrackerReadGuard<'a> {
    guard: RwLockReadGuard<'a, Inner>,
}

impl<'a> TrackerReadGuard<'a> {
    pub fn get_index(&'a self, index: Index) -> Option<&'a TrackerKey> {
        self.guard.get_index(index)
    }

    pub fn iter_keys(&'a self, indexes: &'a Indexes) -> impl Iterator<Item = Option<&'a TrackerKey>> {
        indexes.0.iter().cloned().map(|index| self.get_index(index))
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use kaspa_math::Uint256;

    fn create_addresses(start: usize, count: usize) -> Vec<Address> {
        (start..start + count)
            .map(|i| Address::new(Prefix::Mainnet, kaspa_addresses::Version::PubKey, &Uint256::from_u64(i as u64).to_le_bytes()))
            .collect()
    }

    #[test]
    fn mixed_tracker_keys_share_indexes_reference_counts_and_recycling() {
        let tracker = Tracker::default();
        let id = CovenantId::from_bytes([1; 32]);
        let spk = ScriptPublicKey::from_vec(0, id.as_bytes().to_vec());
        let targets = vec![TrackerKey::Spk(spk.clone()), TrackerKey::Cov(id)];
        let mut first = Indexes::new(vec![]);
        let mut second = Indexes::new(vec![]);
        assert_eq!(tracker.register(&mut first, targets.clone()).unwrap(), targets);
        let spk_index = tracker.get(&spk).unwrap().0;
        let cov_index = tracker.get(&id).unwrap().0;
        assert_ne!(spk_index, cov_index);
        assert_eq!(tracker.len(), 2);

        assert!(tracker.register(&mut first, vec![TrackerKey::Cov(id), TrackerKey::Cov(id)]).unwrap().is_empty());
        assert_eq!(tracker.get(&id).unwrap().1, 1);
        tracker.register(&mut second, vec![TrackerKey::Cov(id)]).unwrap();
        assert_eq!(tracker.get(&id).unwrap().1, 2);
        tracker.unregister(&mut first, vec![TrackerKey::Spk(spk.clone())]);

        let new_id = CovenantId::from_u64_word(2);
        tracker.register(&mut first, vec![TrackerKey::Cov(new_id)]).unwrap();
        assert_eq!(tracker.get(&new_id).unwrap().0, spk_index);
        assert_eq!(tracker.get(&id), Some((cov_index, 2)));
        assert!(tracker.get(&spk).is_none());
        assert!(tracker.matches(&first, &spk, Some(new_id)));
        assert!(tracker.matches(&second, &spk, Some(id)));
        assert!(!tracker.matches(&second, &spk, Some(new_id)));

        tracker.unregister_indexes(&mut first);
        tracker.unregister_indexes(&mut second);
        assert!(tracker.is_empty());
    }

    #[test]
    fn mixed_tracker_registration_rolls_back_new_keys_and_existing_counter_increments() {
        let tracker = Tracker::new(Some(6));
        let mut holder = Indexes::new(vec![]);
        let address = create_addresses(0, 1).remove(0);
        let spk = TrackerKey::from(address.clone());
        let cov_keys: Vec<_> = (1..=5).map(|id| TrackerKey::Cov(CovenantId::from_u64_word(id))).collect();
        let mut targets = vec![spk.clone()];
        targets.extend(cov_keys.clone());
        tracker.register(&mut holder, targets).unwrap();
        let mut counters = Counters::new();
        tracker.register(&mut counters, vec![spk.clone()]).unwrap();
        let index = tracker.get_address(&address).unwrap().0;
        tracker.unregister(&mut holder, vec![cov_keys[4].clone()]);

        let new_id = CovenantId::from_u64_word(6);
        let error = tracker
            .register(&mut counters, vec![spk, TrackerKey::Cov(new_id), TrackerKey::Cov(CovenantId::from_u64_word(7))])
            .unwrap_err();
        assert!(matches!(error, Error::MaxCapacityReached));
        assert_eq!(tracker.get_address(&address), Some((index, 2)));
        assert_eq!(counters.iter().find(|(i, _)| **i == index).map(|(_, count)| *count), Some(1));
        assert_eq!(tracker.get(&new_id).unwrap().1, 0);
        assert_eq!(tracker.len(), 5);
    }

    #[test]
    fn test_tracker_capacity_and_entry_recycling() {
        const INIT_MAX_ADDRESSES: usize = 6;
        const MAX_ADDRESSES: usize = ((INIT_MAX_ADDRESSES + 1) * 8 / 7).next_power_of_two() * 7 / 8 - 1;
        const CAPACITY: usize = MAX_ADDRESSES + 1;

        let tracker = Tracker::new(Some(MAX_ADDRESSES));
        assert_eq!(
            tracker.addresses_preallocation().unwrap(),
            MAX_ADDRESSES,
            "tracker maximum address count should be expanded to the available allocated entries, minus 1 for a transient insert/swap_remove"
        );
        assert_eq!(
            tracker.capacity(),
            CAPACITY,
            "tracker capacity should match the maximum address count plus 1 extra entry for a transient insert/swap_remove"
        );
        let aa = create_addresses(0, MAX_ADDRESSES);
        assert_eq!(aa.len(), MAX_ADDRESSES);

        // Register addresses 0..MAX_ADDRESSES
        let mut idx_a = Indexes::new(vec![]);
        let aa = tracker.register(&mut idx_a, aa).unwrap();
        let aai = aa.iter().map(|x| tracker.get_address(x).unwrap().0).collect_vec();
        assert_eq!(aa.len(), MAX_ADDRESSES, "all addresses should be registered");
        assert_eq!(idx_a.len(), MAX_ADDRESSES, "all addresses should be registered");
        for i in 0..aa.len() {
            assert!(tracker.contains_address(&idx_a, &aa[i]), "tracker should contain the registered address");
            assert!(idx_a.contains(aai[i]), "index set should contain the registered address index");
        }
        assert_eq!(tracker.capacity(), CAPACITY);

        // Try to re-register addresses 0..MAX_ADDRESSES
        let a = tracker.register(&mut idx_a, aa).unwrap();
        assert_eq!(a.len(), 0, "all addresses should already be registered");
        assert_eq!(idx_a.len(), MAX_ADDRESSES, "all addresses should still be registered");

        // Try to register an additional address while the tracker is full
        assert!(
            tracker.register(&mut idx_a, create_addresses(MAX_ADDRESSES, 1)).is_err(),
            "the tracker is full and should refuse a new address"
        );

        // Register address set 1..MAX_ADDRESSES, already fully covered by the tracker address set
        const AB_COUNT: usize = MAX_ADDRESSES - 1;
        let mut idx_b = Indexes::new(vec![]);
        let ab = tracker.register(&mut idx_b, create_addresses(1, AB_COUNT)).unwrap();
        assert_eq!(ab.len(), AB_COUNT, "all addresses should be registered");
        assert_eq!(idx_b.len(), AB_COUNT, "all addresses should be registered");

        // Empty the tracker entry containing A0
        assert_eq!(tracker.unregister(&mut idx_a, create_addresses(0, 1)).len(), 1);
        assert_eq!(idx_a.len(), MAX_ADDRESSES - 1, "entry #0 with address A0 should now be marked empty");

        // Fill the empty entry with a single new address A8
        const AC_COUNT: usize = 1;
        let ac = tracker.register(&mut idx_a, create_addresses(MAX_ADDRESSES, AC_COUNT)).unwrap();
        let aci = ac.iter().map(|x| tracker.get_address(x).unwrap().0).collect_vec();
        assert_eq!(ac.len(), AC_COUNT, "a new address should be registered");
        assert_eq!(idx_a.len(), MAX_ADDRESSES, "a new address should be registered");
        assert_eq!(ac[0], create_addresses(MAX_ADDRESSES, AC_COUNT)[0], "the new address A8 should be registered");
        assert!(tracker.contains_address(&idx_a, &ac[0]), "the new address A8 should be registered");
        assert_eq!(aai[0], aci[0], "the newly registered address A8 should occupy the previously emptied entry");

        assert_eq!(
            tracker.capacity(),
            CAPACITY,
            "the tracker capacity should not have been affected by the transient insert/swap_remove"
        );
    }

    #[test]
    fn test_indexes_eq() {
        let i1 = IndexSet::new(vec![0, 1, 2, 3, 5, 7, 11]);
        let i2 = IndexSet::new(vec![5, 7, 11, 0, 1, 2, 3]);
        let i3 = IndexSet::new(vec![0, 1, 2, 4, 8, 16, 32]);
        let i4 = IndexSet::new(vec![0, 1]);
        assert_eq!(i1, i1);
        assert_eq!(i1, i2);
        assert_ne!(i1, i3);
        assert_ne!(i1, i4);
        assert_eq!(i2, i2);
        assert_ne!(i2, i3);
        assert_ne!(i2, i4);
        assert_eq!(i3, i3);
        assert_ne!(i3, i4);
        assert_eq!(i4, i4);
    }

    #[test]
    fn test_register_rolls_back_existing_counter_increment_on_failure() {
        const MAX_ADDRESSES: usize = 6;

        let tracker = Tracker::new(Some(MAX_ADDRESSES));
        let addresses = create_addresses(0, MAX_ADDRESSES + 1);
        let mut indexes = Indexes::new(vec![]);
        tracker.register(&mut indexes, addresses[..MAX_ADDRESSES].to_vec()).unwrap();

        let address = addresses[0].clone();
        let index = tracker.get_address(&address).unwrap().0;
        let mut counters = CounterMap::new();
        tracker.register(&mut counters, vec![address.clone()]).unwrap();

        let error = tracker.register(&mut counters, vec![address.clone(), addresses[MAX_ADDRESSES].clone()]).unwrap_err();

        assert!(matches!(error, Error::MaxCapacityReached));
        assert_eq!(counters.iter().find(|(entry, _)| **entry == index).map(|(_, count)| *count), Some(1));
        assert_eq!(tracker.get_address(&address).unwrap().1, 2);
    }

    #[test]
    fn test_index_map_replace() {
        let mut m: IndexMap<u64, RefCount> = IndexMap::with_capacity(7);
        m.insert(1, 10);
        m.insert(2, 0);
        m.insert(3, 30);
        m.insert(4, 40);
        assert_eq!(m.get_index(0), Some((&1, &10)));
        assert_eq!(m.get_index(1), Some((&2, &0)));
        assert_eq!(m.get_index(2), Some((&3, &30)));
        assert_eq!(m.get_index(3), Some((&4, &40)));

        assert_eq!(m.swap_remove_index(1), Some((2, 0)));

        assert_eq!(m.get_index(0), Some((&1, &10)));
        assert_eq!(m.get_index(1), Some((&4, &40)));
        assert_eq!(m.get_index(2), Some((&3, &30)));
    }

    #[test]
    fn test_index_map_capacity() {
        const CAPACITY: usize = 14;
        let mut m: IndexMap<u64, RefCount> = IndexMap::with_capacity(CAPACITY);
        for i in 0..CAPACITY {
            m.insert(i as u64, 0);
            assert_eq!(m.capacity(), CAPACITY);
        }
        m.insert(CAPACITY as u64 + 1, 0);
        assert_eq!(m.capacity(), ((CAPACITY + 1) * 8 / 7).next_power_of_two() * 7 / 8);
    }
}
