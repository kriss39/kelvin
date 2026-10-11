//! Turns `getBlock` responses into column vectors (one `Vec` per table column) that `db::write`
//! binds straight to `UNNEST`. Address columns hold an [`Addr`] (a position in
//! `Batch::addresses`) until `db::write` swaps in the database ids.

use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::decode;
use crate::model::{Block, Header, Instruction};

/// Position of an address in `Batch::addresses`.
pub type Addr = u32;

#[derive(Default)]
pub struct Batch {
    /// Distinct addresses referenced by this batch.
    pub addresses: Vec<[u8; 32]>,
    addr_pos: HashMap<[u8; 32], Addr>,
    pub blocks: Blocks,
    pub txs: Txs,
    pub accounts: TxAccounts,
    pub ixs: Instructions,
    pub transfers: Transfers,
    pub logs: Logs,
    pub missing: Vec<Missing>,
    /// Every height up to and including this one is in the batch or in `missing`.
    pub through: Option<u64>,
    pub stats: Stats,
}

pub struct Missing {
    pub from: u64,
    pub to: u64,
    pub reason: String,
}

#[derive(Default)]
pub struct Blocks {
    pub height: Vec<i64>,
    pub time_s: Vec<i64>,
    pub tx_count: Vec<i32>,
    pub sys_tx_count: Vec<i32>,
    pub fingerprint: Vec<i64>,
}

#[derive(Default)]
pub struct Txs {
    pub height: Vec<i64>,
    pub idx: Vec<i32>,
    pub signature: Vec<Vec<u8>>,
    pub fee: Vec<i64>,
    pub err: Vec<Option<String>>,
    pub cu: Vec<Option<i64>>,
    pub valid_from_ms: Vec<Option<i64>>,
    pub fee_payer: Vec<Addr>,
    pub num_signers: Vec<i16>,
    pub num_ro_signed: Vec<i16>,
    pub num_ro_unsigned: Vec<i16>,
    pub account_keys: Vec<Vec<Addr>>,
}

#[derive(Default)]
pub struct TxAccounts {
    pub address: Vec<Addr>,
    pub height: Vec<i64>,
    pub idx: Vec<i32>,
    pub role: Vec<i16>,
}

#[derive(Default)]
pub struct Instructions {
    pub height: Vec<i64>,
    pub idx: Vec<i32>,
    pub ix_idx: Vec<i16>,
    pub inner_idx: Vec<i16>,
    pub program: Vec<Addr>,
    pub kind: Vec<Option<String>>,
    pub data: Vec<Vec<u8>>,
    pub accounts: Vec<Vec<u8>>,
}

#[derive(Default)]
pub struct Transfers {
    pub height: Vec<i64>,
    pub idx: Vec<i32>,
    pub ix_idx: Vec<i16>,
    pub inner_idx: Vec<i16>,
    pub source: Vec<Addr>,
    pub dest: Vec<Addr>,
    /// `None` for native RLO.
    pub mint: Vec<Option<Addr>>,
    /// Decimal strings, cast to NUMERIC in SQL.
    pub amount: Vec<String>,
}

#[derive(Default)]
pub struct Logs {
    pub height: Vec<i64>,
    pub idx: Vec<i32>,
    pub lines: Vec<String>,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub blocks: u64,
    pub user_txs: u64,
    pub sys_txs: u64,
    pub failed_txs: u64,
    pub ixs: u64,
    pub ixs_decoded: u64,
    pub transfers: u64,
    /// Instructions without a decoder, by program id.
    pub undecoded: HashMap<String, u64>,
}

impl Stats {
    pub fn add(&mut self, o: &Stats) {
        self.blocks += o.blocks;
        self.user_txs += o.user_txs;
        self.sys_txs += o.sys_txs;
        self.failed_txs += o.failed_txs;
        self.ixs += o.ixs;
        self.ixs_decoded += o.ixs_decoded;
        self.transfers += o.transfers;
        for (p, n) in &o.undecoded {
            *self.undecoded.entry(p.clone()).or_default() += n;
        }
    }
}

fn pubkey(s: &str) -> Result<[u8; 32]> {
    let b = bs58::decode(s).into_vec().with_context(|| format!("bad base58 address {s:?}"))?;
    b.as_slice().try_into().with_context(|| format!("address {s:?} is {} bytes, expected 32", b.len()))
}

fn signature(s: &str) -> Result<Vec<u8>> {
    let b = bs58::decode(s).into_vec().with_context(|| format!("bad base58 signature {s:?}"))?;
    ensure!(b.len() == 64, "signature {s:?} is {} bytes, expected 64", b.len());
    Ok(b)
}

/// First 8 bytes of sha256 over the block's decoded signatures, as stored in `blocks.fingerprint`.
pub fn fingerprint(b: &Block) -> Result<i64> {
    let mut fp = Sha256::new();
    for s in &b.signatures {
        fp.update(signature(s)?);
    }
    Ok(i64::from_be_bytes(fp.finalize()[..8].try_into().expect("sha256 is 32 bytes")))
}

/// 0 signer+writable, 1 signer+readonly, 2 writable, 3 readonly (Solana message header rules).
pub fn role(i: usize, n: usize, h: &Header) -> i16 {
    let signers = h.num_required_signatures as usize;
    if i < signers {
        if i >= signers.saturating_sub(h.num_readonly_signed_accounts as usize) { 1 } else { 0 }
    } else if i >= n.saturating_sub(h.num_readonly_unsigned_accounts as usize) {
        3
    } else {
        2
    }
}

impl Batch {
    fn intern(&mut self, key: [u8; 32]) -> Addr {
        *self.addr_pos.entry(key).or_insert_with(|| {
            self.addresses.push(key);
            (self.addresses.len() - 1) as Addr
        })
    }

    /// Adds a block. On error the batch is left exactly as it was, so the caller can skip the block.
    pub fn push(&mut self, b: &Block) -> Result<()> {
        let lens = self.lens();
        let res = self.push_unchecked(b);
        if res.is_err() {
            self.truncate(lens);
        }
        res
    }

    fn lens(&self) -> [usize; 6] {
        [
            self.blocks.height.len(),
            self.txs.height.len(),
            self.accounts.height.len(),
            self.ixs.height.len(),
            self.transfers.height.len(),
            self.logs.height.len(),
        ]
    }

    // Addresses interned by a failed push stay (at worst they get ids nothing refers to), and
    // `stats` may count part of the rejected block.
    fn truncate(&mut self, [blocks, txs, accounts, ixs, transfers, logs]: [usize; 6]) {
        let b = &mut self.blocks;
        b.height.truncate(blocks);
        b.time_s.truncate(blocks);
        b.tx_count.truncate(blocks);
        b.sys_tx_count.truncate(blocks);
        b.fingerprint.truncate(blocks);
        let t = &mut self.txs;
        t.height.truncate(txs);
        t.idx.truncate(txs);
        t.signature.truncate(txs);
        t.fee.truncate(txs);
        t.err.truncate(txs);
        t.cu.truncate(txs);
        t.valid_from_ms.truncate(txs);
        t.fee_payer.truncate(txs);
        t.num_signers.truncate(txs);
        t.num_ro_signed.truncate(txs);
        t.num_ro_unsigned.truncate(txs);
        t.account_keys.truncate(txs);
        let a = &mut self.accounts;
        a.address.truncate(accounts);
        a.height.truncate(accounts);
        a.idx.truncate(accounts);
        a.role.truncate(accounts);
        let i = &mut self.ixs;
        i.height.truncate(ixs);
        i.idx.truncate(ixs);
        i.ix_idx.truncate(ixs);
        i.inner_idx.truncate(ixs);
        i.program.truncate(ixs);
        i.kind.truncate(ixs);
        i.data.truncate(ixs);
        i.accounts.truncate(ixs);
        let t = &mut self.transfers;
        t.height.truncate(transfers);
        t.idx.truncate(transfers);
        t.ix_idx.truncate(transfers);
        t.inner_idx.truncate(transfers);
        t.source.truncate(transfers);
        t.dest.truncate(transfers);
        t.mint.truncate(transfers);
        t.amount.truncate(transfers);
        let l = &mut self.logs;
        l.height.truncate(logs);
        l.idx.truncate(logs);
        l.lines.truncate(logs);
    }

    fn push_unchecked(&mut self, b: &Block) -> Result<()> {
        let height = b.block_height as i64;
        let fingerprint = fingerprint(b)?;

        let mut sys_txs = 0;
        for (idx, t) in b.transactions.iter().enumerate() {
            let idx = idx as i32;
            let msg = &t.transaction.message;
            let program_of = |ix: &Instruction| {
                msg.account_keys
                    .get(ix.program_id_index as usize)
                    .map(String::as_str)
                    .with_context(|| format!("tx {idx}: programIdIndex {} out of range", ix.program_id_index))
            };
            let top_programs = msg.instructions.iter().map(program_of).collect::<Result<Vec<_>>>()?;
            if !top_programs.is_empty() && top_programs.iter().all(|p| *p == decode::TOKENOMICS) {
                sys_txs += 1;
                continue;
            }

            let meta = &t.meta;
            let keys = msg.account_keys.iter().map(|k| Ok(self.intern(pubkey(k)?))).collect::<Result<Vec<_>>>()?;
            let sig = t.transaction.signatures.first().with_context(|| format!("tx {idx} has no signature"))?;
            let ok = meta.err.is_none();

            self.txs.height.push(height);
            self.txs.idx.push(idx);
            self.txs.signature.push(signature(sig)?);
            self.txs.fee.push(meta.fee as i64);
            self.txs.err.push(meta.err.as_ref().map(|e| e.to_string()));
            self.txs.cu.push(meta.compute_units_consumed.map(|c| c as i64));
            self.txs.valid_from_ms.push(t.transaction.valid_from);
            self.txs.fee_payer.push(*keys.first().with_context(|| format!("tx {idx} has no accounts"))?);
            self.txs.num_signers.push(msg.header.num_required_signatures.into());
            self.txs.num_ro_signed.push(msg.header.num_readonly_signed_accounts.into());
            self.txs.num_ro_unsigned.push(msg.header.num_readonly_unsigned_accounts.into());
            self.txs.account_keys.push(keys.clone());

            let mut invoked = vec![false; keys.len()];
            let all_ixs =
                msg.instructions.iter().chain(meta.inner_instructions.iter().flatten().map(|i| &i.instruction));
            for ix in all_ixs {
                // An out-of-range inner index is reported by program_of below.
                if let Some(slot) = invoked.get_mut(ix.program_id_index as usize) {
                    *slot = true;
                }
            }
            for (i, &k) in keys.iter().enumerate().filter(|(i, _)| !invoked[*i]) {
                self.accounts.address.push(k);
                self.accounts.height.push(height);
                self.accounts.idx.push(idx);
                self.accounts.role.push(role(i, keys.len(), &msg.header));
            }

            for (ix_idx, ix) in msg.instructions.iter().enumerate() {
                self.push_ix(height, idx, ix_idx as i16, -1, ix, top_programs[ix_idx], &keys, ok)?;
            }
            // Rialo lists inner instructions flat; number them within their parent instruction.
            let mut next_inner: HashMap<u8, i16> = HashMap::new();
            for inner in meta.inner_instructions.iter().flatten() {
                let pos = next_inner.entry(inner.instruction_index).or_default();
                let program = program_of(&inner.instruction)?;
                self.push_ix(
                    height,
                    idx,
                    inner.instruction_index.into(),
                    *pos,
                    &inner.instruction,
                    program,
                    &keys,
                    ok,
                )?;
                *pos += 1;
            }

            if let Some(lines) = meta.log_messages.as_ref().filter(|l| !l.is_empty()) {
                self.logs.height.push(height);
                self.logs.idx.push(idx);
                self.logs.lines.push(lines.join("\n"));
            }

            self.stats.user_txs += 1;
            self.stats.failed_txs += u64::from(!ok);
        }

        self.blocks.height.push(height);
        self.blocks.time_s.push(b.block_time);
        self.blocks.tx_count.push(b.transactions.len() as i32);
        self.blocks.sys_tx_count.push(sys_txs);
        self.blocks.fingerprint.push(fingerprint);
        self.stats.blocks += 1;
        self.stats.sys_txs += sys_txs as u64;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_ix(
        &mut self,
        height: i64,
        idx: i32,
        ix_idx: i16,
        inner_idx: i16,
        ix: &Instruction,
        program: &str,
        keys: &[Addr],
        ok: bool,
    ) -> Result<()> {
        let data =
            bs58::decode(&ix.data).into_vec().with_context(|| format!("tx {idx} ix {ix_idx}: bad base58 data"))?;
        let decoded = decode::decode(program, &data);

        self.stats.ixs += 1;
        match &decoded {
            Some(_) => self.stats.ixs_decoded += 1,
            None => *self.stats.undecoded.entry(program.to_owned()).or_default() += 1,
        }

        if let (true, Some(t)) = (ok, decoded.as_ref().and_then(|d| d.transfer.as_ref())) {
            let key = |pos: usize| {
                ix.accounts
                    .get(pos)
                    .and_then(|&a| keys.get(a as usize).copied())
                    .with_context(|| format!("tx {idx} ix {ix_idx}: transfer account #{pos} out of range"))
            };
            let (source, dest) = (key(t.source)?, key(t.dest)?);
            let mint = t.mint.map(key).transpose()?;
            self.transfers.height.push(height);
            self.transfers.idx.push(idx);
            self.transfers.ix_idx.push(ix_idx);
            self.transfers.inner_idx.push(inner_idx);
            self.transfers.source.push(source);
            self.transfers.dest.push(dest);
            self.transfers.mint.push(mint);
            self.transfers.amount.push(t.amount.to_string());
            self.stats.transfers += 1;
        }

        self.ixs.height.push(height);
        self.ixs.idx.push(idx);
        self.ixs.ix_idx.push(ix_idx);
        self.ixs.inner_idx.push(inner_idx);
        // program_of has already checked the index.
        self.ixs.program.push(keys[ix.program_id_index as usize]);
        self.ixs.kind.push(decoded.map(|d| d.kind.to_owned()));
        self.ixs.data.push(data);
        self.ixs.accounts.push(ix.accounts.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Batch;
    use crate::model::Block;

    struct Expect {
        user_txs: usize,
        sys_txs: u64,
        transfers: usize,
        token_transfers: usize,
        inner: usize,
        accounts: usize,
        fingerprint: i64,
        kinds: &'static str,
    }

    // Real testnet blocks; the expectations match what the indexer stored for them.
    fn check(json: &str, e: Expect) -> Batch {
        let block: Block = serde_json::from_str(json).expect("fixture parses");
        let mut b = Batch::default();
        b.push(&block).expect("block normalizes");
        assert_eq!(b.txs.height.len(), e.user_txs, "user transactions");
        assert_eq!(b.stats.sys_txs, e.sys_txs, "system transactions");
        assert_eq!(b.transfers.amount.len(), e.transfers, "transfers");
        assert_eq!(b.transfers.mint.iter().filter(|m| m.is_some()).count(), e.token_transfers, "token transfers");
        assert_eq!(b.ixs.inner_idx.iter().filter(|&&i| i >= 0).count(), e.inner, "inner instructions");
        assert_eq!(b.accounts.address.len(), e.accounts, "tx_accounts rows");
        assert_eq!(b.blocks.fingerprint, vec![e.fingerprint]);
        let kinds: Vec<&str> = b.ixs.kind.iter().map(|k| k.as_deref().unwrap_or("-")).collect();
        assert_eq!(kinds.join(","), e.kinds);
        b
    }

    #[test]
    fn plain_transfer() {
        check(
            include_str!("../tests/fixtures/block_16802722.json"),
            Expect {
                user_txs: 1,
                sys_txs: 1,
                transfers: 1,
                token_transfers: 0,
                inner: 0,
                accounts: 2,
                fingerprint: 4566755088743683761,
                kinds: "transfer",
            },
        );
    }

    #[test]
    fn failed_transaction_moves_nothing() {
        let b = check(
            include_str!("../tests/fixtures/block_16635879.json"),
            Expect {
                user_txs: 2,
                sys_txs: 1,
                transfers: 1,
                token_transfers: 0,
                inner: 0,
                accounts: 9,
                fingerprint: 2625571836112769084,
                kinds: "transfer,-",
            },
        );
        let failed: Vec<i32> = (0..b.txs.idx.len()).filter(|&i| b.txs.err[i].is_some()).map(|i| b.txs.idx[i]).collect();
        assert_eq!(failed.len(), 1);
        assert!(!b.transfers.idx.contains(&failed[0]), "a failed transaction records no transfers");
    }

    #[test]
    fn oracle_report_with_inner_account_creation() {
        check(
            include_str!("../tests/fixtures/block_16636172.json"),
            Expect {
                user_txs: 1,
                sys_txs: 1,
                transfers: 2,
                token_transfers: 0,
                inner: 2,
                accounts: 3,
                fingerprint: -785171530000799208,
                kinds: "set_compute_unit_limit,set_loaded_accounts_data_size_limit,report,create_account,create_account",
            },
        );
    }

    #[test]
    fn token_2022_transfer_with_associated_account() {
        let b = check(
            include_str!("../tests/fixtures/block_17131902.json"),
            Expect {
                user_txs: 1,
                sys_txs: 1,
                transfers: 2,
                token_transfers: 1,
                inner: 4,
                accounts: 5,
                fingerprint: -8899623210557161263,
                kinds: "create_idempotent,transfer_checked,get_account_data_size,create_account,\
                        initialize_immutable_owner,initialize_account3",
            },
        );
        // Top-level instructions come first: the token amount with its mint, then the rent the
        // inner create_account paid in RLO for the new token account.
        assert_eq!(b.transfers.amount, ["10000000000", "2074080"]);
        assert_eq!(b.transfers.mint[1], None);
        let mint = b.transfers.mint[0].expect("token transfer has a mint");
        assert!(bs58::encode(b.addresses[mint as usize]).into_string().starts_with("M4Uzk6ws"));
    }
}
