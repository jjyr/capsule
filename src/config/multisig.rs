use super::MultisigLock;
use ckb_tool::ckb_jsonrpc_types::{JsonBytes, Transaction};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MultisigConfig {
    pub transaction: Transaction,
    pub lock: MultisigLock,
    pub signatures: Vec<JsonBytes>,
}
