use super::{errors::BuilderResult, policy::Policy};
use crate::{block_template::selector::TransactionsSelector, model::candidate_tx::CandidateTransaction};
use kaspa_consensus_core::{
    api::ConsensusApi,
    block::{BlockTemplate, TemplateBuildMode},
    coinbase::MinerData,
    merkle::calc_hash_merkle_root,
    tx::COINBASE_TRANSACTION_INDEX,
};
use kaspa_core::{
    debug,
    time::{unix_now, Stopwatch},
};

pub(crate) struct BlockTemplateBuilder {
    policy: Policy,
}

impl BlockTemplateBuilder {
    pub(crate) fn new(max_block_mass: u64) -> Self {
        let policy = Policy::new(max_block_mass);
        Self { policy }
    }

    /// BuildBlockTemplate creates a block template for a miner to consume
    /// BuildBlockTemplate returns a new block template that is ready to be solved
    /// using the transactions from the passed transaction source pool and a coinbase
    /// that either pays to the passed address if it is not nil, or a coinbase that
    /// is redeemable by anyone if the passed address is nil. The nil address
    /// functionality is useful since there are cases such as the get_block_template
    /// RPC where external mining software is responsible for creating their own
    /// coinbase which will replace the one generated for the block template. Thus
    /// the need to have configured address can be avoided.
    ///
    /// The transactions selected and included are prioritized according to several
    /// factors. First, each transaction has a priority calculated based on its
    /// value, age of inputs, and size. Transactions which consist of larger
    /// amounts, older inputs, and small sizes have the highest priority. Second, a
    /// fee per kilobyte is calculated for each transaction. Transactions with a
    /// higher fee per kilobyte are preferred. Finally, the block generation related
    /// policy settings are all taken into account.
    ///
    /// Transactions which only spend outputs from other transactions already in the
    /// block DAG are immediately added to a priority queue which either
    /// prioritizes based on the priority (then fee per kilobyte) or the fee per
    /// kilobyte (then priority) depending on whether or not the BlockPrioritySize
    /// policy setting allots space for high-priority transactions. Transactions
    /// which spend outputs from other transactions in the source pool are added to a
    /// dependency map so they can be added to the priority queue once the
    /// transactions they depend on have been included.
    ///
    /// Once the high-priority area (if configured) has been filled with
    /// transactions, or the priority falls below what is considered high-priority,
    /// the priority queue is updated to prioritize by fees per kilobyte (then
    /// priority).
    ///
    /// When the fees per kilobyte drop below the TxMinFreeFee policy setting, the
    /// transaction will be skipped unless the BlockMinSize policy setting is
    /// nonzero, in which case the block will be filled with the low-fee/free
    /// transactions until the block size reaches that minimum size.
    ///
    /// Any transactions which would cause the block to exceed the BlockMaxMass
    /// policy setting, exceed the maximum allowed signature operations per block, or
    /// otherwise cause the block to be invalid are skipped.
    ///
    /// Given the above, a block generated by this function is of the following form:
    ///
    ///   -----------------------------------  --  --
    ///  |      Coinbase Transaction         |   |   |
    ///  |-----------------------------------|   |   |
    ///  |                                   |   |   | ----- policy.BlockPrioritySize
    ///  |   High-priority Transactions      |   |   |
    ///  |                                   |   |   |
    ///  |-----------------------------------|   | --
    ///  |                                   |   |
    ///  |                                   |   |
    ///  |                                   |   |--- policy.BlockMaxMass
    ///  |  Transactions prioritized by fee  |   |
    ///  |  until <= policy.TxMinFreeFee     |   |
    ///  |                                   |   |
    ///  |                                   |   |
    ///  |                                   |   |
    ///  |-----------------------------------|   |
    ///  |  Low-fee/Non high-priority (free) |   |
    ///  |  transactions (while block size   |   |
    ///  |  <= policy.BlockMinSize)          |   |
    ///   -----------------------------------  --
    pub(crate) fn build_block_template(
        &self,
        consensus: &dyn ConsensusApi,
        miner_data: &MinerData,
        transactions: Vec<CandidateTransaction>,
        build_mode: TemplateBuildMode,
    ) -> BuilderResult<BlockTemplate> {
        let _sw = Stopwatch::<100>::with_threshold("build_block_template op");
        debug!("Considering {} transactions for a new block template", transactions.len());
        let selector = Box::new(TransactionsSelector::new(self.policy.clone(), transactions));
        Ok(consensus.build_block_template(miner_data.clone(), selector, build_mode)?)
    }

    /// modify_block_template clones an existing block template, modifies it to the requested coinbase data and updates the timestamp
    pub(crate) fn modify_block_template(
        consensus: &dyn ConsensusApi,
        new_miner_data: &MinerData,
        block_template_to_modify: &BlockTemplate,
    ) -> BuilderResult<BlockTemplate> {
        let mut block_template = block_template_to_modify.clone();

        // The first transaction is always the coinbase transaction
        let coinbase_tx = &mut block_template.block.transactions[COINBASE_TRANSACTION_INDEX];
        let new_payload = consensus.modify_coinbase_payload(coinbase_tx.payload.clone(), new_miner_data)?;
        coinbase_tx.payload = new_payload;
        if block_template.coinbase_has_red_reward {
            // The last output is always the coinbase red blocks reward
            coinbase_tx.outputs.last_mut().unwrap().script_public_key = new_miner_data.script_public_key.clone();
        }
        // Update the hash merkle root according to the modified transactions
        block_template.block.header.hash_merkle_root = calc_hash_merkle_root(block_template.block.transactions.iter());
        let new_timestamp = unix_now();
        if new_timestamp > block_template.block.header.timestamp {
            // Only if new time stamp is later than current, update the header. Otherwise,
            // we keep the previous time as built by internal consensus median time logic
            block_template.block.header.timestamp = new_timestamp;
        }
        block_template.block.header.finalize();
        block_template.miner_data = new_miner_data.clone();
        Ok(block_template)
    }
}
