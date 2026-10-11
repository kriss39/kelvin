//! `getBlock` response as Rialo 0.21.0-alpha.0 returns it. Differences from Solana:
//! - `blockTime` is seconds here but milliseconds in other methods;
//! - `meta` has no pre/post balances;
//! - `innerInstructions` is a flat list of `{instructionIndex, instruction}`, not grouped.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub block_height: u64,
    pub block_time: i64,
    pub transactions: Vec<TxWithMeta>,
    pub signatures: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TxWithMeta {
    pub transaction: Transaction,
    pub meta: Meta,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub signatures: Vec<String>,
    pub message: Message,
    /// Milliseconds.
    pub valid_from: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub account_keys: Vec<String>,
    pub header: Header,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub num_required_signatures: u8,
    pub num_readonly_signed_accounts: u8,
    pub num_readonly_unsigned_accounts: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instruction {
    pub accounts: Vec<u8>,
    /// base58
    pub data: String,
    pub program_id_index: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub err: Option<Value>,
    pub fee: u64,
    pub log_messages: Option<Vec<String>>,
    pub inner_instructions: Option<Vec<InnerInstruction>>,
    pub compute_units_consumed: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerInstruction {
    pub instruction_index: u8,
    pub instruction: Instruction,
}
