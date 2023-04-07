use crate::{tx::TransactionId, BlockHashMap};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcceptanceData {
    pub merged_blocks: BlockHashMap<Vec<AcceptedTxEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedTxEntry {
    pub transaction_id: TransactionId,
    pub index_within_block: u32,
}
