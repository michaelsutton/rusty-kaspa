use super::feerate_key::FeerateTransactionKey;
use crate::block_template::selector::ALPHA;
use arg::FeerateWeight;
use indexmap::IndexSet;
use itertools::Either;
use kaspa_utils::{rand::seq::index, vec::VecExtensions};
use rand::{distributions::Uniform, prelude::Distribution, Rng};
use std::collections::{BTreeSet, HashSet};
use sweep_bptree::BPlusTreeMap;

pub mod arg {
    use crate::block_template::selector::ALPHA;
    use sweep_bptree::tree::{Argument, SearchArgument};

    type FeerateKey = super::FeerateTransactionKey;

    #[derive(Clone, Copy, Debug, Default)]
    pub struct FeerateWeight(f64);

    impl FeerateWeight {
        /// Returns the weight value
        pub fn weight(&self) -> f64 {
            self.0
        }
    }

    impl Argument<FeerateKey> for FeerateWeight {
        fn from_leaf(keys: &[FeerateKey]) -> Self {
            Self(keys.iter().map(|k| k.feerate().powi(ALPHA)).sum())
        }

        fn from_inner(_keys: &[FeerateKey], arguments: &[Self]) -> Self {
            Self(arguments.iter().map(|a| a.0).sum())
        }
    }

    impl SearchArgument<FeerateKey> for FeerateWeight {
        type Query = f64;

        fn locate_in_leaf(query: Self::Query, keys: &[FeerateKey]) -> Option<usize> {
            let mut sum = 0.0;
            for (i, k) in keys.iter().enumerate() {
                let w = k.feerate().powi(ALPHA);
                sum += w;
                if query <= sum {
                    return Some(i);
                }
            }
            None
        }

        fn locate_in_inner(mut query: Self::Query, _keys: &[FeerateKey], arguments: &[Self]) -> Option<(usize, Self::Query)> {
            for (i, a) in arguments.iter().enumerate() {
                if query >= a.0 {
                    query -= a.0;
                } else {
                    return Some((i, query));
                }
            }
            None
        }
    }
}

/// Management of the transaction pool frontier, that is, the set of transactions in
/// the transaction pool which have no mempool ancestors and are essentially ready
/// to enter the next block template.
pub struct Frontier {
    /// Frontier transactions sorted by feerate order
    feerate_order: BPlusTreeMap<FeerateTransactionKey, (), FeerateWeight>,

    /// Total sampling weight: Σ_{tx in frontier}(tx.fee/tx.mass)^alpha
    total_weight: f64,

    /// Total masses: Σ_{tx in frontier} tx.mass
    total_mass: u64,
}

impl Default for Frontier {
    fn default() -> Self {
        Self { feerate_order: BPlusTreeMap::new(), total_weight: Default::default(), total_mass: Default::default() }
    }
}

impl Frontier {
    pub fn insert(&mut self, key: FeerateTransactionKey) -> bool {
        let (weight, mass) = (key.feerate().powi(ALPHA), key.mass);
        if self.feerate_order.insert(key, ()).is_none() {
            self.total_weight += weight;
            self.total_mass += mass;
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, key: &FeerateTransactionKey) -> bool {
        let (weight, mass) = (key.feerate().powi(ALPHA), key.mass);
        if self.feerate_order.remove(&key).is_some() {
            self.total_weight -= weight;
            self.total_mass -= mass;
            true
        } else {
            false
        }
    }

    pub fn sample<'a, R>(&'a self, rng: &'a mut R, amount: u32) -> impl Iterator<Item = FeerateTransactionKey> + 'a
    where
        R: Rng + ?Sized,
    {
        let length = self.feerate_order.len() as u32;
        if length <= amount {
            return Either::Left(self.feerate_order.iter().map(|(k, _)| k.clone()));
        }
        let distr = Uniform::new(0f64, self.total_weight);
        let mut cache = HashSet::new();
        Either::Right((0..amount).map(move |_| {
            let query = distr.sample(rng);
            let mut item = self.feerate_order.get_by_argument(query).unwrap().0;
            while !cache.insert(item.tx.id()) {
                let query = distr.sample(rng);
                item = self.feerate_order.get_by_argument(query).unwrap().0;
            }
            item.clone()
        }))
    }

    pub(crate) fn len(&self) -> usize {
        self.feerate_order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::candidate_tx::CandidateTransaction, Policy, TransactionsSelector};
    use itertools::Itertools;
    use kaspa_consensus_core::{
        subnets::SUBNETWORK_ID_NATIVE,
        tx::{Transaction, TransactionInput, TransactionOutpoint},
    };
    use kaspa_hashes::{HasherBase, TransactionID};
    use rand::thread_rng;
    use std::{collections::HashMap, sync::Arc};

    fn generate_unique_tx(i: u64) -> Arc<Transaction> {
        let mut hasher = TransactionID::new();
        let prev = hasher.update(i.to_le_bytes()).clone().finalize();
        let input = TransactionInput::new(TransactionOutpoint::new(prev, 0), vec![], 0, 0);
        Arc::new(Transaction::new(0, vec![input], vec![], 0, SUBNETWORK_ID_NATIVE, 0, vec![]))
    }

    fn stage_two_sampling(container: impl IntoIterator<Item = FeerateTransactionKey>) -> Vec<Transaction> {
        let set = container.into_iter().map(CandidateTransaction::from_key).collect_vec();
        let mut selector = TransactionsSelector::new(Policy::new(500_000), set);
        selector.select_transactions()
    }

    #[test]
    pub fn test_two_stage_sampling() {
        let mut rng = thread_rng();
        let cap = 100_000;
        let mut map = HashMap::with_capacity(cap);
        for i in 0..cap as u64 {
            let fee: u64 = if i % (cap as u64 / 100) == 0 { 1000000 } else { rng.gen_range(1..10000) };
            let mass: u64 = 1650;
            let tx = generate_unique_tx(i);
            map.insert(tx.id(), FeerateTransactionKey { fee: fee.max(mass), mass, tx });
        }

        let len = cap;
        let mut frontier = Frontier::default();
        for item in map.values().take(len).cloned() {
            frontier.insert(item).then_some(()).unwrap();
        }

        let stage_one = frontier.sample(&mut rng, 10_000);
        let stage_two = stage_two_sampling(stage_one);
        stage_two.into_iter().map(|k| k.gas).sum::<u64>();
    }

    #[test]
    fn test_sweep_btree() {
        use sweep_bptree::argument::count::Count;
        use sweep_bptree::BPlusTreeMap;

        // use Count as Argument to create a order statistic tree
        let mut map = BPlusTreeMap::<i32, i32, Count>::new();
        map.insert(1, 2);
        map.insert(2, 3);
        map.insert(3, 4);

        // get by order, time complexity is log(n)
        assert_eq!(map.get_by_argument(0), Some((&1, &2)));
        assert_eq!(map.get_by_argument(1), Some((&2, &3)));

        // get the offset for key

        // 0 does not exists
        assert_eq!(map.rank_by_argument(&0), Err(0));

        assert_eq!(map.rank_by_argument(&1), Ok(0));
        assert_eq!(map.rank_by_argument(&2), Ok(1));
        assert_eq!(map.rank_by_argument(&3), Ok(2));

        // 4 does not exists
        assert_eq!(map.rank_by_argument(&4), Err(3));
    }
}
