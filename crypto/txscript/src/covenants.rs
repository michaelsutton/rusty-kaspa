use kaspa_hashes::Hash;
use kaspa_txscript_errors::TxScriptError;
use std::{collections::HashMap, sync::LazyLock};

/// Context for an input's specific authority over a subset of outputs.
///
/// Used by scripts to verify the state transitions they directly authorized
/// (e.g., 1-to-N splits) without scanning unrelated outputs.
pub struct CovenantInputContext {
    /// The covenant ID shared by this input and its authorized outputs.
    pub covenant_id: Hash,

    /// Indices of outputs that explicitly declare this input as their `authorizing_input`.
    ///
    /// This defines the input's direct "children" in the transaction.
    pub auth_outputs: Vec<usize>,
}

impl CovenantInputContext {
    pub fn new(covenant_id: Hash) -> Self {
        Self { covenant_id, auth_outputs: Default::default() }
    }
}

/// Context for the shared transaction-wide state of a specific Covenant ID.
///
/// Used for verifying global invariants across all participants of the same covenant
/// (e.g., merges, batching, or conservation of amounts).
#[derive(Default)]
pub struct CovenantSharedContext {
    /// Indices of *all* inputs in the transaction carrying this `covenant_id`.
    pub input_indices: Vec<usize>,

    /// Indices of *all* outputs in the transaction carrying this `covenant_id`.
    pub output_indices: Vec<usize>,
}

/// Pre-computed cache mapping inputs and covenant ids to their execution contexts.
///
/// Enables O(1) access for covenant introspection opcodes.
#[derive(Default)]
pub struct CovenantsContext {
    /// Maps an input index to its local authority context.
    pub input_ctxs: HashMap<usize, CovenantInputContext>,

    /// Maps a covenant id to its shared transaction-wide context.
    pub shared_ctxs: HashMap<Hash, CovenantSharedContext>,
}

impl CovenantsContext {
    /// Returns the absolute transaction output index for the K-th authorized output.
    pub(crate) fn auth_output_index(&self, input_idx: usize, k: usize) -> Result<usize, TxScriptError> {
        let auth_outputs = &self.input_ctxs.get(&input_idx).ok_or(TxScriptError::InvalidCovInputIndex(input_idx as i32))?.auth_outputs;
        auth_outputs.get(k).copied().ok_or(TxScriptError::InvalidCovOutIndex(k, input_idx, auth_outputs.len()))
    }

    /// Returns the number of outputs authorized by this input.
    pub(crate) fn num_auth_outputs(&self, input_idx: usize) -> Result<usize, TxScriptError> {
        Ok(self.input_ctxs.get(&input_idx).ok_or(TxScriptError::InvalidCovInputIndex(input_idx as i32))?.auth_outputs.len())
    }
}

pub static EMPTY_COV_CONTEXT: LazyLock<CovenantsContext> = LazyLock::new(CovenantsContext::default);
