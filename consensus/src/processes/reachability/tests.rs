//!
//! Test utils for reachability
//!
use super::{inquirer::*, tree::*};
use crate::{
    model::stores::{
        reachability::{ReachabilityStore, ReachabilityStoreReader},
        relations::{RelationsStore, RelationsStoreReader},
    },
    processes::{
        ghostdag::mergeset::unordered_mergeset_without_selected_parent,
        reachability::interval::Interval,
        relations::{delete_reachability_relations, init as relations_init},
    },
};
use itertools::Itertools;
use kaspa_consensus_core::{
    blockhash::{BlockHashExtensions, BlockHashes, ORIGIN},
    BlockHashMap, BlockHashSet,
};
use kaspa_database::prelude::StoreError;
use kaspa_hashes::Hash;
use std::collections::{
    hash_map::Entry::{Occupied, Vacant},
    VecDeque,
};
use thiserror::Error;

/// A struct with fluent API to streamline reachability store building
pub struct StoreBuilder<'a, T: ReachabilityStore + ?Sized> {
    store: &'a mut T,
}

impl<'a, T: ReachabilityStore + ?Sized> StoreBuilder<'a, T> {
    pub fn new(store: &'a mut T) -> Self {
        Self { store }
    }

    pub fn add_block(&mut self, hash: Hash, parent: Hash) -> &mut Self {
        let parent_height = if !parent.is_none() { self.store.append_child(parent, hash).unwrap() } else { 0 };
        self.store.insert(hash, parent, Interval::empty(), parent_height + 1).unwrap();
        self
    }
}

/// A struct with fluent API to streamline tree building
pub struct TreeBuilder<'a, T: ReachabilityStore + ?Sized> {
    store: &'a mut T,
    reindex_depth: u64,
    reindex_slack: u64,
}

impl<'a, T: ReachabilityStore + ?Sized> TreeBuilder<'a, T> {
    pub fn new(store: &'a mut T) -> Self {
        Self {
            store,
            reindex_depth: crate::constants::perf::DEFAULT_REINDEX_DEPTH,
            reindex_slack: crate::constants::perf::DEFAULT_REINDEX_SLACK,
        }
    }

    pub fn new_with_params(store: &'a mut T, reindex_depth: u64, reindex_slack: u64) -> Self {
        Self { store, reindex_depth, reindex_slack }
    }

    pub fn init(&mut self) -> &mut Self {
        init(self.store).unwrap();
        self
    }

    pub fn init_with_params(&mut self, origin: Hash, capacity: Interval) -> &mut Self {
        init_with_params(self.store, origin, capacity).unwrap();
        self
    }

    pub fn add_block(&mut self, hash: Hash, parent: Hash) -> &mut Self {
        add_tree_block(self.store, hash, parent, self.reindex_depth, self.reindex_slack).unwrap();
        try_advancing_reindex_root(self.store, hash, self.reindex_depth, self.reindex_slack).unwrap();
        self
    }

    pub fn store(&self) -> &&'a mut T {
        &self.store
    }
}

#[derive(Clone)]
pub struct DagBlock {
    pub hash: Hash,
    pub parents: Vec<Hash>,
}

impl DagBlock {
    pub fn new(hash: Hash, parents: Vec<Hash>) -> Self {
        Self { hash, parents }
    }
}

/// A struct with fluent API to streamline DAG building
pub struct DagBuilder<'a, T: ReachabilityStore + ?Sized, S: RelationsStore + ?Sized> {
    store: &'a mut T,
    relations: &'a mut S,
}

impl<'a, T: ReachabilityStore + ?Sized, S: RelationsStore + ?Sized> DagBuilder<'a, T, S> {
    pub fn new(store: &'a mut T, relations: &'a mut S) -> Self {
        Self { store, relations }
    }

    pub fn init(&mut self) -> &mut Self {
        init(self.store).unwrap();
        relations_init(self.relations);
        self
    }

    pub fn delete_block(&mut self, hash: Hash) -> &mut Self {
        let mergeset = delete_reachability_relations(self.relations, self.store, hash);
        delete_block(self.store, hash, &mut mergeset.iter().cloned()).unwrap();
        self
    }

    pub fn add_block(&mut self, block: DagBlock) -> &mut Self {
        // Select by height (longest chain) just for the sake of internal isolated tests
        let selected_parent = block.parents.iter().cloned().max_by_key(|p| self.store.get_height(*p).unwrap()).unwrap();
        let mergeset = unordered_mergeset_without_selected_parent(self.relations, self.store, selected_parent, &block.parents);
        add_block(self.store, block.hash, selected_parent, &mut mergeset.iter().cloned()).unwrap();
        hint_virtual_selected_parent(self.store, block.hash).unwrap();
        self.relations.insert(block.hash, BlockHashes::new(block.parents)).unwrap();
        self
    }

    pub fn store(&self) -> &&'a mut T {
        &self.store
    }
}

/// Validates that relations are consistent and do not contain any dangling hash etc
pub fn validate_relations<S: RelationsStoreReader + ?Sized>(relations: &S) -> std::result::Result<(), TestError> {
    let mut queue = VecDeque::<Hash>::from([ORIGIN]);
    let mut visited: BlockHashSet = queue.iter().copied().collect();
    while let Some(current) = queue.pop_front() {
        let parents = relations.get_parents(current)?;
        assert_eq!(parents.len(), parents.iter().copied().unique_by(|&h| h).count(), "duplicate hashes in parents array");
        for parent in parents.iter().copied() {
            let parent_children = relations.get_children(parent)?;
            assert!(parent_children.contains(&current), "missing child entry");
        }
        let children = relations.get_children(current)?;
        assert_eq!(children.len(), children.iter().copied().unique_by(|&h| h).count(), "duplicate hashes in children array");
        for child in children.iter().copied() {
            if visited.insert(child) {
                queue.push_back(child);
            }
        }
    }
    assert_eq!(relations.counts().unwrap(), (visited.len(), visited.len()));
    Ok(())
}

pub fn subtree<S: ReachabilityStoreReader + ?Sized>(reachability: &S, root: Hash) -> BlockHashSet {
    let mut queue = VecDeque::<Hash>::from([root]);
    let mut vec = Vec::new();
    while let Some(parent) = queue.pop_front() {
        let children = reachability.get_children(parent).unwrap();
        queue.extend(children.iter());
        vec.extend(children.iter());
    }
    let len = vec.len();
    let set: BlockHashSet = vec.into_iter().collect();
    assert_eq!(len, set.len());
    set
}

pub fn inclusive_past<S: RelationsStoreReader + ?Sized>(relations: &S, hash: Hash) -> BlockHashSet {
    let mut queue = VecDeque::<Hash>::from([hash]);
    let mut visited: BlockHashSet = queue.iter().copied().collect();
    while let Some(current) = queue.pop_front() {
        let parents = relations.get_parents(current).unwrap();
        for parent in parents.iter().copied() {
            if parent != ORIGIN && visited.insert(parent) {
                queue.push_back(parent);
            }
        }
    }
    visited
}

pub fn build_transitive_closure_ref<S: RelationsStoreReader + ?Sized>(relations: &S, hashes: &[Hash]) -> TransitiveClosure {
    let mut closure = TransitiveClosure::new();
    for x in hashes.iter().copied() {
        let past = inclusive_past(relations, x);
        for y in hashes.iter().copied() {
            closure.set(x, y, past.contains(&y));
        }
    }
    closure
}

pub fn build_transitive_closure<S: RelationsStoreReader + ?Sized, V: ReachabilityStoreReader + ?Sized>(
    relations: &S,
    reachability: &V,
    hashes: &[Hash],
) -> TransitiveClosure {
    let mut closure = TransitiveClosure::new();
    for x in hashes.iter().copied() {
        for y in hashes.iter().copied() {
            closure.set(x, y, is_dag_ancestor_of(reachability, y, x).unwrap());
        }
    }
    let expected_closure = build_transitive_closure_ref(relations, hashes);
    assert_eq!(expected_closure, closure);
    closure
}

pub fn build_chain_closure_ref<S: ReachabilityStoreReader + ?Sized>(reachability: &S, hashes: &[Hash]) -> TransitiveClosure {
    let mut closure = TransitiveClosure::new();
    for x in hashes.iter().copied() {
        let subtree = subtree(reachability, x);
        for y in hashes.iter().copied() {
            closure.set(x, y, x == y || subtree.contains(&y));
        }
    }
    closure
}

pub fn build_chain_closure<V: ReachabilityStoreReader + ?Sized>(reachability: &V, hashes: &[Hash]) -> TransitiveClosure {
    let mut closure = TransitiveClosure::new();
    for x in hashes.iter().copied() {
        for y in hashes.iter().copied() {
            closure.set(x, y, is_chain_ancestor_of(reachability, x, y).unwrap());
        }
    }
    let expected_closure = build_chain_closure_ref(reachability, hashes);
    assert_eq!(expected_closure, closure);
    closure
}

#[derive(PartialEq, Eq, Debug, Default)]
pub struct TransitiveClosure {
    matrix: BlockHashMap<BlockHashMap<bool>>,
}

impl TransitiveClosure {
    pub fn new() -> Self {
        Self { matrix: Default::default() }
    }

    pub fn set(&mut self, x: Hash, y: Hash, b: bool) {
        let row = match self.matrix.entry(x) {
            Occupied(e) => e.into_mut(),
            Vacant(e) => e.insert(Default::default()),
        };

        if let Vacant(e) = row.entry(y) {
            e.insert(b);
        } else {
            panic!()
        }
    }

    pub fn get(&self, x: Hash, y: Hash) -> Option<bool> {
        Some(*self.matrix.get(&x)?.get(&y)?)
    }

    pub fn subset_of(&self, other: &TransitiveClosure) -> bool {
        for (x, row) in self.matrix.iter() {
            for (y, val) in row.iter() {
                if let Some(other_val) = other.get(*x, *y) {
                    if other_val != *val {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Error, Debug)]
pub enum TestError {
    #[error("data store error")]
    StoreError(#[from] StoreError),

    #[error("empty interval")]
    EmptyInterval(Hash, Interval),

    #[error("sibling intervals are expected to be consecutive")]
    NonConsecutiveSiblingIntervals(Interval, Interval),

    #[error("child interval out of parent bounds")]
    IntervalOutOfParentBounds { parent: Hash, child: Hash, parent_interval: Interval, child_interval: Interval },
}

pub trait StoreValidationExtensions {
    /// Checks if `block` is in the past of `other` (creates hashes from the u64 numbers)
    fn in_past_of(&self, block: u64, other: u64) -> bool;

    /// Checks if `block` and `other` are in the anticone of each other
    /// (creates hashes from the u64 numbers)
    fn are_anticone(&self, block: u64, other: u64) -> bool;

    /// Validates that all tree intervals match the expected interval relations
    fn validate_intervals(&self, root: Hash) -> std::result::Result<(), TestError>;
}

impl<T: ReachabilityStoreReader + ?Sized> StoreValidationExtensions for T {
    fn in_past_of(&self, block: u64, other: u64) -> bool {
        if block == other {
            return false;
        }
        let res = is_dag_ancestor_of(self, block.into(), other.into()).unwrap();
        if res {
            // Assert that the `future` relation is indeed asymmetric
            assert!(!is_dag_ancestor_of(self, other.into(), block.into()).unwrap())
        }
        res
    }

    fn are_anticone(&self, block: u64, other: u64) -> bool {
        !is_dag_ancestor_of(self, block.into(), other.into()).unwrap()
            && !is_dag_ancestor_of(self, other.into(), block.into()).unwrap()
    }

    fn validate_intervals(&self, root: Hash) -> std::result::Result<(), TestError> {
        let mut queue = VecDeque::<Hash>::from([root]);
        while let Some(parent) = queue.pop_front() {
            let children = self.get_children(parent)?;
            queue.extend(children.iter());

            let parent_interval = self.get_interval(parent)?;
            if parent_interval.is_empty() {
                return Err(TestError::EmptyInterval(parent, parent_interval));
            }

            // Verify parent-child strict relation
            for child in children.iter().cloned() {
                let child_interval = self.get_interval(child)?;
                if !parent_interval.strictly_contains(child_interval) {
                    return Err(TestError::IntervalOutOfParentBounds { parent, child, parent_interval, child_interval });
                }
            }

            // Iterate over consecutive siblings
            for siblings in children.windows(2) {
                let sibling_interval = self.get_interval(siblings[0])?;
                let current_interval = self.get_interval(siblings[1])?;
                if sibling_interval.end + 1 != current_interval.start {
                    return Err(TestError::NonConsecutiveSiblingIntervals(sibling_interval, current_interval));
                }
            }
        }
        Ok(())
    }
}
