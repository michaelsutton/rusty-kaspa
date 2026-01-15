use std::{
    ops::{ControlFlow, DerefMut},
    sync::{atomic::Ordering, Arc},
};

use itertools::Itertools;
use kaspa_consensus_core::{
    blockhash::{BlockHashExtensions, BlockHashes, ORIGIN},
    errors::pruning::{ProofWeakness, PruningImportError, PruningImportResult},
    header::Header,
    pruning::{PruningPointProof, PruningProofMetadata},
    BlockLevel, BlueWorkType,
};
use kaspa_core::info;
use kaspa_database::{
    prelude::{CachePolicy, ConnBuilder, StoreResultUnitExt},
    utils::DbLifetime,
};
use kaspa_hashes::Hash;
use kaspa_pow::{calc_block_level, calc_block_level_check_pow};
use kaspa_utils::vec::VecExtensions;
use parking_lot::RwLock;
use rocksdb::WriteBatch;

use crate::{
    model::{
        services::reachability::MTReachabilityService,
        stores::{
            ghostdag::{DbGhostdagStore, GhostdagStore, GhostdagStoreReader},
            headers::{DbHeadersStore, HeaderStore, HeaderStoreReader},
            headers_selected_tip::HeadersSelectedTipStoreReader,
            reachability::{DbReachabilityStore, ReachabilityStoreReader},
            relations::{DbRelationsStore, RelationsStoreReader},
        },
    },
    processes::{
        ghostdag::protocol::GhostdagManager, pruning_proof::GhostdagReaderExt, reachability::inquirer as reachability,
        relations::RelationsStoreExtensions,
    },
};

use super::PruningProofManager;

struct ProofContext {
    headers_store: Arc<DbHeadersStore>,
    ghostdag_stores: Vec<Arc<DbGhostdagStore>>,
    relations_stores: Vec<DbRelationsStore>,
    reachability_stores: Vec<Arc<RwLock<DbReachabilityStore>>>,
    ghostdag_managers:
        Vec<GhostdagManager<DbGhostdagStore, DbRelationsStore, MTReachabilityService<DbReachabilityStore>, DbHeadersStore>>,
    selected_tip_by_level: Vec<Hash>,

    pp_header: Arc<Header>,
    pp_level: BlockLevel,

    db_lifetime: DbLifetime,
}

struct ProofLevelContext<'a> {
    ghostdag_store: &'a DbGhostdagStore,
    selected_tip: Hash,
}

impl ProofLevelContext<'_> {
    /// Returns an option of the hash of the challenger and defender's common ancestor at this level.
    /// If no such ancestor exists, returns None.
    fn find_common_ancestor(challenger: &Self, defender: &Self) -> Option<Hash> {
        let mut current = challenger.selected_tip;
        let mut challenger_gd_of_current = challenger.ghostdag_store.get_compact_data(current).unwrap();
        loop {
            if defender.ghostdag_store.has(current).unwrap() {
                break Some(current);
            } else {
                current = challenger_gd_of_current.selected_parent;
                if current.is_origin() {
                    break None;
                }
                challenger_gd_of_current = challenger.ghostdag_store.get_compact_data(current).unwrap();
            };
        }
    }

    /// Returns the blue work difference between the level selected tip and `ancestor`
    fn blue_work_diff(&self, ancestor: Hash) -> BlueWorkType {
        self.ghostdag_store
            .get_blue_work(self.selected_tip)
            .unwrap()
            .saturating_sub(self.ghostdag_store.get_blue_work(ancestor).unwrap())
    }

    /// Returns the overall blue score for this level (essentially the level selected tip blue score)
    fn blue_score(&self) -> u64 {
        self.ghostdag_store.get_blue_score(self.selected_tip).unwrap()
    }
}

impl ProofContext {
    /// Build the full context from the proof
    fn from_proof(
        ppm: &PruningProofManager,
        proof: &PruningPointProof,
        log_validating: bool,
    ) -> Result<ControlFlow<(), ProofContext>, PruningImportError> {
        if proof.len() != ppm.max_block_level as usize + 1 {
            return Err(PruningImportError::ProofNotEnoughLevels(ppm.max_block_level as usize + 1));
        }

        if proof[0].is_empty() {
            return Err(PruningImportError::PruningProofNotEnoughHeaders);
        }

        let ghostdag_k = ppm.ghostdag_k;

        let headers_estimate = ppm.estimate_proof_unique_size(proof);

        //
        // Initialize stores
        //

        let (db_lifetime, db) = kaspa_database::create_temp_db!(ConnBuilder::default().with_files_limit(10));
        let cache_policy = CachePolicy::Count(2 * ppm.pruning_proof_m as usize);
        let headers_store =
            Arc::new(DbHeadersStore::new(db.clone(), CachePolicy::Count(headers_estimate), CachePolicy::Count(headers_estimate)));
        let ghostdag_stores = (0..=ppm.max_block_level)
            .map(|level| Arc::new(DbGhostdagStore::new(db.clone(), level, cache_policy, cache_policy)))
            .collect_vec();
        let mut relations_stores =
            (0..=ppm.max_block_level).map(|level| DbRelationsStore::new(db.clone(), level, cache_policy, cache_policy)).collect_vec();
        let reachability_stores = (0..=ppm.max_block_level)
            .map(|level| Arc::new(RwLock::new(DbReachabilityStore::with_block_level(db.clone(), cache_policy, cache_policy, level))))
            .collect_vec();

        let reachability_services = (0..=ppm.max_block_level)
            .map(|level| MTReachabilityService::new(reachability_stores[level as usize].clone()))
            .collect_vec();

        let ghostdag_managers = ghostdag_stores
            .iter()
            .cloned()
            .enumerate()
            .map(|(level, ghostdag_store)| {
                GhostdagManager::with_level(
                    ppm.genesis_hash,
                    ghostdag_k,
                    ghostdag_store,
                    relations_stores[level].clone(),
                    headers_store.clone(),
                    reachability_services[level].clone(),
                    level as BlockLevel,
                    ppm.max_block_level,
                )
            })
            .collect_vec();

        {
            let mut batch = WriteBatch::default();
            for level in 0..=ppm.max_block_level {
                let level = level as usize;
                reachability::init(reachability_stores[level].write().deref_mut()).unwrap();
                relations_stores[level].insert_batch(&mut batch, ORIGIN, BlockHashes::new(vec![])).unwrap();
                ghostdag_stores[level].insert(ORIGIN, ghostdag_managers[level].origin_ghostdag_data()).unwrap();
            }

            db.write(batch).unwrap();
        }

        let proof_pp_header = proof[0].last().expect("checked if empty").clone();
        let proof_pp_level = calc_block_level(&proof_pp_header, ppm.max_block_level);
        let proof_pp = proof_pp_header.hash;

        //
        // Populate stores
        //

        let mut selected_tip_by_level = vec![None; ppm.max_block_level as usize + 1];
        for level in (0..=ppm.max_block_level).rev() {
            // Before processing this level, check if the process is exiting so we can end early
            if ppm.is_consensus_exiting.load(Ordering::Relaxed) {
                return Ok(ControlFlow::Break(()));
            }

            if log_validating {
                info!("Validating level {level} from the pruning point proof ({} headers)", proof[level as usize].len());
            }
            let level_idx = level as usize;
            let mut selected_tip =
                proof[level as usize].first().map(|header| header.hash).ok_or(PruningImportError::PruningProofNotEnoughHeaders)?;
            for (i, header) in proof[level as usize].iter().enumerate() {
                let (header_level, pow_passes) = calc_block_level_check_pow(header, ppm.max_block_level);
                if header_level < level {
                    return Err(PruningImportError::PruningProofWrongBlockLevel(header.hash, header_level, level));
                }
                if !ppm.skip_proof_of_work && !pow_passes {
                    return Err(PruningImportError::ProofOfWorkFailed(header.hash, level));
                }

                headers_store.insert(header.hash, header.clone(), header_level).idempotent().unwrap();

                // Filter out parents that do not appear at the pruning proof:
                let parents = ppm
                    .parents_manager
                    .parents_at_level(header, level)
                    .iter()
                    .copied()
                    .filter(|parent| ghostdag_stores[level_idx].has(*parent).unwrap())
                    .collect_vec();

                // Only the first block at each level is allowed to have no known parents
                if parents.is_empty() && i != 0 {
                    return Err(PruningImportError::PruningProofHeaderWithNoKnownParents(header.hash, level));
                }

                for &parent in parents.iter() {
                    if headers_store.get_header(parent).unwrap().blue_work >= header.blue_work {
                        return Err(PruningImportError::PruningProofInconsistentBlueWork(header.hash, level));
                    }
                }

                let parents: BlockHashes = parents.push_if_empty(ORIGIN).into();

                if relations_stores[level_idx].has(header.hash).unwrap() {
                    return Err(PruningImportError::PruningProofDuplicateHeaderAtLevel(header.hash, level));
                }

                relations_stores[level_idx].insert(header.hash, parents.clone()).unwrap();
                let ghostdag_data = Arc::new(ghostdag_managers[level_idx].ghostdag(&parents));
                ghostdag_stores[level_idx].insert(header.hash, ghostdag_data.clone()).unwrap();

                // Update the selected tip
                selected_tip = ghostdag_managers[level_idx].find_selected_parent([selected_tip, header.hash]);

                let mut level_reachability = reachability_stores[level_idx].write();
                let mut reachability_mergeset = ghostdag_data
                    .unordered_mergeset_without_selected_parent()
                    .filter(|hash| level_reachability.has(*hash).unwrap())
                    .collect_vec()
                    .into_iter();

                reachability::add_block(
                    level_reachability.deref_mut(),
                    header.hash,
                    ghostdag_data.selected_parent,
                    &mut reachability_mergeset,
                )
                .unwrap();

                if selected_tip == header.hash {
                    reachability::hint_virtual_selected_parent(level_reachability.deref_mut(), header.hash).unwrap();
                }
                drop(level_reachability);
            }

            if level < ppm.max_block_level {
                let block_at_depth_m_at_next_level = ghostdag_stores[level_idx + 1]
                    .block_at_depth(selected_tip_by_level[level_idx + 1].unwrap(), ppm.pruning_proof_m)
                    .unwrap();
                if !relations_stores[level_idx].has(block_at_depth_m_at_next_level).unwrap() {
                    return Err(PruningImportError::PruningProofMissingBlockAtDepthMFromNextLevel(level, level + 1));
                }
            }

            // The selected tip at a given level must be anchored to the pruning point:
            // - At levels ≤ the pruning-point level, the selected tip must be the pruning point itself.
            // - At higher levels, it must be a parent of the pruning point at that level.
            if level <= proof_pp_level {
                if selected_tip != proof_pp {
                    return Err(PruningImportError::PruningProofSelectedTipIsNotThePruningPoint(selected_tip, level));
                }
            } else if !ppm.parents_manager.parents_at_level(&proof_pp_header, level).contains(&selected_tip) {
                return Err(PruningImportError::PruningProofSelectedTipNotParentOfPruningPoint(selected_tip, level));
            }

            let tip_blue_score = ghostdag_stores[level_idx].get_blue_score(selected_tip).expect("tip expected");
            let level_root = proof[level_idx].first().expect("checked earlier").hash;
            if level_root != ppm.genesis_hash && tip_blue_score < 2 * ppm.pruning_proof_m {
                return Err(PruningImportError::PruningProofSelectedTipNotEnoughBlueScore(selected_tip, level, tip_blue_score));
            }

            selected_tip_by_level[level_idx] = Some(selected_tip);
        }

        let selected_tip_by_level = selected_tip_by_level.into_iter().map(|selected_tip| selected_tip.unwrap()).collect();

        let ctx = ProofContext {
            db_lifetime,
            headers_store,
            ghostdag_stores,
            relations_stores,
            reachability_stores,
            ghostdag_managers,
            selected_tip_by_level,
            pp_header: proof_pp_header,
            pp_level: proof_pp_level,
        };

        Ok(ControlFlow::Continue(ctx))
    }

    /// Returns a per-level context
    fn level(&self, level: BlockLevel) -> ProofLevelContext<'_> {
        ProofLevelContext {
            ghostdag_store: &self.ghostdag_stores[level as usize],
            selected_tip: self.selected_tip_by_level[level as usize],
        }
    }
}

impl PruningProofManager {
    /// Validates an incoming pruning point proof against the current consensus.
    ///
    /// The function reconstructs temporary stores for both the
    /// challenger proof and the current (defender) consensus, validates all
    /// selected tips, and compares blue work including pruning-period work.
    ///
    /// Returns `Ok(())` if the proof is valid and superior, or an appropriate
    /// `PruningImportError` otherwise.
    pub fn validate_pruning_point_proof(
        &self,
        proof: &PruningPointProof,
        proof_metadata: &PruningProofMetadata,
    ) -> PruningImportResult<()> {
        // Initialize the stores for the incoming pruning proof (the challenger)
        let challenger =
            ProofContext::from_proof(self, proof, true)?.continue_value().ok_or(PruningImportError::PruningValidationInterrupted)?;

        // Get the proof for the current consensus (the defender) and recreate the stores for it
        // This is expected to be fast because if a proof exists, it will be cached.
        // If no proof exists, this is empty
        let mut defender_proof = self.get_pruning_point_proof();
        if defender_proof.is_empty() {
            // An empty proof can only happen if we're at genesis. We're going to create a proof for this case that contains the genesis header only
            let genesis_header = self.headers_store.get_header(self.genesis_hash).unwrap();
            defender_proof = Arc::new((0..=self.max_block_level).map(|_| vec![genesis_header.clone()]).collect_vec());
        }
        let defender = ProofContext::from_proof(self, &defender_proof, false)
            .expect("local")
            .continue_value()
            .ok_or(PruningImportError::PruningValidationInterrupted)?;

        Ok(self.compare_proofs_inner(
            defender,
            challenger,
            self.headers_selected_tip_store.read().get().unwrap().blue_work,
            proof_metadata.relay_block_blue_work,
        )?)
    }

    /// Compares two MLS pruning proofs and determines whether the challenger supersedes the defender.
    ///
    /// The comparison is performed level-by-level, considering only levels that satisfy the
    /// ≥2M threshold. When a common ancestor exists at a given level, the proofs are
    /// compared by their accumulated blue work from that ancestor onward, including the
    /// respective pruning-period work; otherwise, if no common ancestor is found, the
    /// challenger is considered better only if it possesses a qualifying level where the
    /// defender does not.
    ///
    /// The challenger is considered better only if it is *strictly* superior according to
    /// these criteria. In case of equality, or when no strict advantage can be established,
    /// the defender is favored to preserve stability.
    fn compare_proofs_inner(
        &self,
        defender: ProofContext,
        challenger: ProofContext,
        defender_relay_blue_work: BlueWorkType,
        challenger_relay_blue_work: BlueWorkType,
    ) -> Result<(), ProofWeakness> {
        // The accumulated blue work of the defender's proof from the pruning point onward
        let defender_pruning_period_work = defender_relay_blue_work.saturating_sub(defender.pp_header.blue_work);

        // The claimed blue work of the challenger's proof from their pruning point and up to the triggering relay block. This work
        // will eventually be verified if the proof is accepted so we can treat it as trusted
        let challenger_claimed_pruning_period_work = challenger_relay_blue_work.saturating_sub(challenger.pp_header.blue_work);

        for level in 0..=self.max_block_level {
            // Init level ctxs
            let challenger_level_ctx = challenger.level(level);
            let defender_level_ctx = defender.level(level);

            // Next check is to see if the challenger's proof is "better" than the defender's
            // Step 1 - look only at levels that have a full proof (at least 2M blocks)
            if challenger_level_ctx.blue_score() < 2 * self.pruning_proof_m {
                continue;
            }

            // Step 2 - if a common ancestor exists between the challenger and defender proofs,
            // compare their accumulated blue work from that ancestor onward.
            // The challenger proof is better iff the blue work difference from the ancestor
            // to the challenger's selected tip, plus its pruning-period work, is strictly
            // greater than the corresponding defender value.
            if let Some(common_ancestor) = ProofLevelContext::find_common_ancestor(&challenger_level_ctx, &defender_level_ctx) {
                if defender_level_ctx.blue_work_diff(common_ancestor).saturating_add(defender_pruning_period_work)
                    >= challenger_level_ctx.blue_work_diff(common_ancestor).saturating_add(challenger_claimed_pruning_period_work)
                {
                    return Err(ProofWeakness::InsufficientBlueWork);
                }

                return Ok(());
            }
        }

        if defender.pp_header.hash == self.genesis_hash {
            // If the challenger has better tips and the defender's pruning point is still
            // genesis, we consider the challenger to be better.
            return Ok(());
        }

        // If we got here it means there's no level with shared blocks
        // between the challenger and the defender. In this case we
        // consider the challenger to be better if it has at least one level
        // with 2M blue blocks where the defender doesn't.
        for level in (0..=self.max_block_level).rev() {
            if challenger.level(level).blue_score() < 2 * self.pruning_proof_m {
                continue;
            }
            if defender.level(level).blue_score() < 2 * self.pruning_proof_m {
                return Ok(());
            }
        }

        drop(challenger);
        drop(defender);

        Err(ProofWeakness::NotEnoughHeaders)
    }

    /// Compares two MLS pruning proofs and determines whether the challenger supersedes the defender.
    ///
    /// See [`PruningProofManager::compare_proofs_inner`] for more details.
    ///
    /// Exposed here for internal revalidation needs.
    pub(crate) fn compare_proofs(
        &self,
        defender: &PruningPointProof,
        challenger: &PruningPointProof,
        defender_relay_blue_work: BlueWorkType,
        challenger_relay_blue_work: BlueWorkType,
    ) -> ControlFlow<(), Result<(), ProofWeakness>> {
        ControlFlow::Continue(self.compare_proofs_inner(
            ProofContext::from_proof(self, defender, false).expect("internal")?,
            ProofContext::from_proof(self, challenger, false).expect("internal")?,
            defender_relay_blue_work,
            challenger_relay_blue_work,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigBuilder;
    use crate::consensus::test_consensus::TestConsensus;
    use crate::processes::reachability::tests::r#gen::generate_complex_dag;
    use kaspa_consensus_core::config::params::SIMNET_PARAMS;
    use kaspa_consensus_core::errors::pruning::PruningImportError;
    use kaspa_pow::calc_block_level;

    fn init_consensus(skip_pow: bool) -> (TestConsensus, crate::config::Config) {
        // init_allocator_with_default_settings();
        let mut builder = ConfigBuilder::new(SIMNET_PARAMS).edit_consensus_params(|p| p.max_block_level = BlockLevel::MAX - 1);
        if skip_pow {
            builder = builder.skip_proof_of_work();
        }
        let cfg = builder.build();
        let consensus = TestConsensus::new(&cfg);
        (consensus, cfg)
    }

    async fn build_dag_into_consensus(consensus: &TestConsensus, genesis: Hash, target_blocks: u64) -> Vec<Hash> {
        // generate_complex_dag returns (something, Vec<(id, parents)>)
        // We map id==0 => actual consensus genesis hash.
        let (genesis_id, nodes) = generate_complex_dag(/*delay=*/ 1.0, /*bps=*/ 15.0, target_blocks);

        let mut inserted = Vec::with_capacity(nodes.len().saturating_sub(1));
        for (id, parents) in nodes {
            if id == genesis_id {
                continue; // genesis already exists in consensus
            }
            let h = id.into();
            let p = parents.into_iter().map(|pid| if pid == genesis_id { genesis } else { pid.into() }).collect::<Vec<_>>();

            // header-only blocks are enough for pruning proof logic
            consensus.add_header_only_block_with_parents(h, p).await.unwrap();
            inserted.push(h);
        }
        inserted
    }

    /// Helper: run from_proof and assert a specific pruning error variant.
    fn assert_from_proof_err<F>(ppm: &PruningProofManager, proof: PruningPointProof, check: F)
    where
        F: FnOnce(PruningImportError),
    {
        match ProofContext::from_proof(ppm, &proof, /*log_validating=*/ false) {
            Err(e) => check(e),
            Ok(ControlFlow::Break(())) => panic!("expected Err(..) but got ControlFlow::Break"),
            Ok(ControlFlow::Continue(_)) => panic!("expected Err(..) but got Ok(Continue)"),
        }
    }

    #[tokio::test]
    async fn proof_context_from_proof_all_error_invariants() {
        //
        // 1) Main consensus with skip_pow=true (easier to build lots of blocks)
        //
        let (consensus, cfg) = init_consensus(/*skip_pow=*/ true);
        let wait_handles = consensus.init();

        let genesis = cfg.genesis.hash;

        // Build a fairly large DAG so we get some natural low-level sampling.
        // Increase if your pruning_proof_m is very large.
        let inserted_hashes = build_dag_into_consensus(&consensus, genesis, /*target_blocks=*/ 8000).await;

        // Grab pruning proof manager
        // (Assumes consensus has services.pruning_proof_manager like other managers used in TestConsensus.)
        let ppm = &consensus.consensus_clone().services.pruning_proof_manager;

        let m = ppm.pruning_proof_m;
        let max_level = ppm.max_block_level;

        // Pick pruning points:
        // - pp_low: non-genesis with blue_score < 2m (to trigger SelectedTipNotEnoughBlueScore)
        // - pp_good: non-genesis, low level (< max) and blue_score >= 2m+5 (to produce a valid proof)
        let mut pp_low: Option<Hash> = None;
        let mut pp_good: Option<Hash> = None;

        for &h in &inserted_hashes {
            let hdr = ppm.headers_store.get_header(h).unwrap();
            let lvl = calc_block_level(&hdr, max_level);
            let bs = hdr.blue_score;

            // We want a pruning point that is NOT genesis and has a lower block level than max,
            // so that we have levels > pp_level and can hit the "NotParentOfPruningPoint" invariant.
            if h != genesis && lvl < max_level {
                if pp_low.is_none() && bs > 0 && bs < 2 * m {
                    pp_low = Some(h);
                }
                if pp_good.is_none() && bs >= 2 * m + 5 {
                    pp_good = Some(h);
                }
            }

            if pp_low.is_some() && pp_good.is_some() {
                break;
            }
        }

        let pp_good = pp_good.unwrap_or_else(|| {
            // Fallback: pick the last inserted block (usually highest blue score)
            *inserted_hashes.last().expect("need at least one non-genesis block")
        });

        let pp_low = pp_low.unwrap_or_else(|| {
            // Fallback: pick the earliest inserted non-genesis block (usually low blue score)
            *inserted_hashes.first().expect("need at least one non-genesis block")
        });

        //
        // Build a VALID proof (baseline)
        //
        let proof_good = ppm.build_pruning_point_proof(pp_good);
        let base = ProofContext::from_proof(ppm, &proof_good, false)
            .expect("baseline should not error")
            .continue_value()
            .expect("baseline should not break");

        //
        // Now: mutate proof_good to hit every error invariant (except PoW fail handled separately below)
        //

        // A) ProofNotEnoughLevels
        {
            let mut p = proof_good.clone();
            p.pop();
            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::ProofNotEnoughLevels(expected) => {
                    assert_eq!(expected, max_level as usize + 1);
                }
                other => panic!("expected ProofNotEnoughLevels, got {other:?}"),
            });
        }

        // B) PruningProofNotEnoughHeaders (proof[0] empty)
        {
            let mut p = proof_good.clone();
            p[0].clear();
            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofNotEnoughHeaders => {}
                other => panic!("expected PruningProofNotEnoughHeaders, got {other:?}"),
            });
        }

        // C) PruningProofWrongBlockLevel: place a level-0 header into a higher level bucket
        {
            // Find a header in the baseline proof with computed level == 0
            let mut low_lvl_hdr: Option<Arc<Header>> = None;
            for h in proof_good.iter().flatten() {
                let lvl = calc_block_level(h, max_level);
                if lvl == 0 {
                    low_lvl_hdr = Some(h.clone());
                    break;
                }
            }
            let low_lvl_hdr = low_lvl_hdr.expect("need at least one naturally sampled level-0 header; increase blocks or max level");

            let bad_level: BlockLevel = 1;
            let mut p = proof_good.clone();
            assert!(!p[bad_level as usize].is_empty());
            p[bad_level as usize][0] = low_lvl_hdr;

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofWrongBlockLevel(_, header_level, level_expected) => {
                    assert!(header_level < level_expected);
                    assert_eq!(level_expected, bad_level);
                }
                other => panic!("expected PruningProofWrongBlockLevel, got {other:?}"),
            });
        }

        // D) PruningProofHeaderWithNoKnownParents: for i!=0, make all parents unknown so filtered parents = empty
        {
            let level: BlockLevel = 0;
            let mut p = proof_good.clone();
            if p[level as usize].len() < 2 {
                panic!("need at least 2 headers at some level to trigger HeaderWithNoKnownParents; try increasing blocks");
            }

            let mut hdr = (*p[level as usize][1]).clone();
            // Put only unknown parents at every level so parents_at_level(..., level) returns unknown and gets filtered out.
            hdr.parents_by_level = (0..=max_level).map(|_| vec![999_999_999u64.into()]).collect_vec().try_into().unwrap();
            p[level as usize][1] = Arc::new(hdr);

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofHeaderWithNoKnownParents(_, lvl) => assert_eq!(lvl, level),
                other => panic!("expected PruningProofHeaderWithNoKnownParents, got {other:?}"),
            });
        }

        // E) PruningProofInconsistentBlueWork: set header.blue_work <= parent.blue_work
        {
            // Find a level with at least 2 headers and where the 2nd has at least one known parent
            let mut chosen: Option<(BlockLevel, usize)> = None;
            'outer: for lvl in 0..=max_level {
                let v = &proof_good[lvl as usize];
                if v.len() < 2 {
                    continue;
                }
                // We rely on existing parents; just pick index 1 and hope it has some known parent in proof.
                chosen = Some((lvl, 1));
                break 'outer;
            }
            let (lvl, idx) = chosen.expect("could not find a suitable level for inconsistent blue work");

            let mut p = proof_good.clone();
            let mut hdr = (*p[lvl as usize][idx]).clone();
            hdr.blue_work = 0.into(); // almost certainly <= any parent blue_work
            p[lvl as usize][idx] = Arc::new(hdr);

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofInconsistentBlueWork(_, level) => assert_eq!(level, lvl),
                other => panic!("expected PruningProofInconsistentBlueWork, got {other:?}"),
            });
        }

        // F) PruningProofDuplicateHeaderAtLevel: duplicate the first header within a level vector
        {
            let lvl: BlockLevel = 0;
            let mut p = proof_good.clone();
            let h1 = p[lvl as usize][1].clone();
            p[lvl as usize].insert(2, h1);

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofDuplicateHeaderAtLevel(_, level) => assert_eq!(level, lvl),
                other => panic!("expected PruningProofDuplicateHeaderAtLevel, got {other:?}"),
            });
        }

        // G) PruningProofMissingBlockAtDepthMFromNextLevel:
        // remove the "block at depth M" (computed from baseline ctx) from the lower level proof
        {
            // pick a level that is < max_level
            let lvl: BlockLevel = (max_level - 1).min(1);
            let next = lvl + 1;

            let target = base.ghostdag_stores[next as usize].block_at_depth(base.selected_tip_by_level[next as usize], m).unwrap();

            let mut p = proof_good.clone();
            let before = p[lvl as usize].len();
            p[lvl as usize].retain(|h| h.hash != target);

            // Ensure we actually removed something and didn't empty the level
            if p[lvl as usize].len() == before || p[lvl as usize].is_empty() {
                panic!("failed to remove depth-M target from level {lvl}; try another level or increase blocks");
            }

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofMissingBlockAtDepthMFromNextLevel(l0, l1) => {
                    assert_eq!(l0, lvl);
                    assert_eq!(l1, next);
                }
                other => panic!("expected PruningProofMissingBlockAtDepthMFromNextLevel, got {other:?}"),
            });
        }

        // H) PruningProofSelectedTipIsNotThePruningPoint:
        // Change proof[0].last() (the PP header) so level-0 selected tip won't match it.
        {
            let mut p = proof_good.clone();
            if p[1].len() < 2 {
                panic!("need at least 2 headers at level 0 to swap pruning point header");
            }
            let len = p[1].len();
            p[1].swap(len - 2, len - 1);

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofSelectedTipIsNotThePruningPoint(_, lvl) => assert_eq!(lvl, 1),
                other => panic!("expected PruningProofSelectedTipIsNotThePruningPoint, got {other:?}"),
            });
        }

        // I) PruningProofSelectedTipNotParentOfPruningPoint:
        // At some level > pp_level, force selected tip to be a non-parent by adding a "better" competing header.
        {
            // We want a high level where this invariant is checked as "level > pp_level".
            // Use max_level (or any high level).
            let lvl: BlockLevel = max_level;

            let mut p = proof_good.clone();

            // Take an existing header at this level and clone it into a new "competing tip" with a new hash and huge blue_work.
            let base_hdr = (*p[lvl as usize][0]).clone();
            let mut new_hdr = base_hdr.clone();
            new_hdr.hash = 8_888_888_888u64.into();
            new_hdr.blue_work = base_hdr.blue_work.saturating_add(1_000_000_000.into());
            new_hdr.blue_score = base_hdr.blue_score.saturating_add(1_000_000_000);

            p[lvl as usize].push(Arc::new(new_hdr));

            assert_from_proof_err(ppm, p, |e| match e {
                PruningImportError::PruningProofSelectedTipNotParentOfPruningPoint(_, level) => assert_eq!(level, lvl),
                other => panic!("expected PruningProofSelectedTipNotParentOfPruningPoint, got {other:?}"),
            });
        }

        // J) PruningProofSelectedTipNotEnoughBlueScore:
        // Build a proof around a pruning point with blue_score < 2m (non-genesis).
        {
            let proof_low = ppm.build_pruning_point_proof(pp_low);
            assert_from_proof_err(ppm, proof_low, |e| match e {
                PruningImportError::PruningProofSelectedTipNotEnoughBlueScore(_, _, tip_bs) => {
                    assert!(tip_bs < 2 * m, "expected tip blue score < 2m, got {tip_bs}, m={m}");
                }
                other => panic!("expected PruningProofSelectedTipNotEnoughBlueScore, got {other:?}"),
            });
        }

        //
        // 2) Separate consensus with skip_pow=false just to hit ProofOfWorkFailed deterministically
        //
        {
            let (consensus_pow, cfg_pow) = init_consensus(/*skip_pow=*/ false);
            let wait_handles_pow = consensus_pow.init();

            let ppm_pow = &consensus_pow.consensus_clone().services.pruning_proof_manager;
            let max_level_pow = ppm_pow.max_block_level;
            let genesis_pow = cfg_pow.genesis.hash;
            let genesis_header = ppm_pow.headers_store.get_header(genesis_pow).unwrap();

            // Craft a pruning point header that:
            // - has genesis as parent at EVERY level (so higher-level selected tip can be genesis and still be "a parent of pp")
            // - lives at level 0 bucket, so WrongBlockLevel doesn't trigger before PoW check
            // - has bits=0 => essentially impossible target, so PoW fails (nonce 0 should fail)
            let mut bad = (*genesis_header).clone();
            bad.hash = 7_777_777_777u64.into();
            bad.bits = 0;
            bad.parents_by_level = (0..=max_level_pow).map(|_| vec![genesis_pow]).collect_vec().try_into().unwrap();

            let bad = Arc::new(bad);

            // Build a "minimal but well-shaped" proof:
            // - for every level>0: use genesis header so selected tip is genesis
            // - for level 0: include the bad header as the pruning point header
            let mut proof = (0..=max_level_pow).map(|_| vec![genesis_header.clone()]).collect_vec();
            proof[0] = vec![bad.clone()];

            assert_from_proof_err(ppm_pow, proof, |e| match e {
                PruningImportError::ProofOfWorkFailed(h, lvl) => {
                    assert_eq!(h, bad.hash);
                    assert_eq!(lvl, 0);
                }
                other => panic!("expected ProofOfWorkFailed, got {other:?}"),
            });

            consensus_pow.shutdown(wait_handles_pow);
        }

        consensus.shutdown(wait_handles);
    }
}
