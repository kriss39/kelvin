//! Labels and decoders for Rialo's native programs. Layouts are Solana's (little-endian; System
//! uses a u32 discriminant, ComputeBudget and the token programs a u8), checked against testnet
//! blocks and their program logs on 2026-10-11.

pub const SYSTEM: &str = "11111111111111111111111111111111";
pub const COMPUTE_BUDGET: &str = "ComputeBudget111111111111111111111111111111";
/// Every block carries one transaction calling only this program ("FreezeStakes").
pub const TOKENOMICS: &str = "TokenomicsGovernance11111111111111111111111";
pub const ORACLE_PROCESSOR: &str = "Qrac1eProcessor1111111111111111111111111111";
pub const ORACLE_REGISTRY: &str = "Qrac1eRegistry11111111111111111111111111111";
pub const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
/// Classic SPL Token; not seen on Rialo yet, but it shares Token-2022's base layout.
pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Programs `decode` understands; `kelvin redecode` revisits stored instructions of these.
pub const DECODABLE: [&str; 7] =
    [SYSTEM, COMPUTE_BUDGET, ORACLE_PROCESSOR, ORACLE_REGISTRY, TOKEN_2022, ASSOCIATED_TOKEN, TOKEN];

pub fn label(program: &str) -> Option<&'static str> {
    Some(match program {
        SYSTEM => "System",
        COMPUTE_BUDGET => "ComputeBudget",
        TOKENOMICS => "TokenomicsGovernance",
        ORACLE_PROCESSOR => "OracleProcessor",
        ORACLE_REGISTRY => "OracleRegistry",
        TOKEN_2022 => "Token2022",
        ASSOCIATED_TOKEN => "AssociatedToken",
        TOKEN => "Token",
        "CryptoSvc1111111111111111111111111111111111" => "CryptoService",
        "TEERegistry11111111111111111111111111111111" => "TEERegistry",
        "FeatureEnf1111111111111111111111111111111111" => "FeatureEnforcer",
        "FeaturePo1icy1111111111111111111111111111111" => "FeaturePolicy",
        _ => return None,
    })
}

#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub kind: &'static str,
    pub transfer: Option<Transfer>,
}

/// A value movement. `source`, `dest` and `mint` are positions in the instruction's account list;
/// `mint` is `None` for native RLO. Token mints are recorded as a transfer from the mint account,
/// burns as a transfer to it.
#[derive(Debug, PartialEq)]
pub struct Transfer {
    pub source: usize,
    pub dest: usize,
    pub mint: Option<usize>,
    pub amount: u64,
}

pub fn decode(program: &str, data: &[u8]) -> Option<Decoded> {
    match program {
        SYSTEM => system(data),
        COMPUTE_BUDGET => compute_budget(data),
        TOKEN_2022 | TOKEN => token(data),
        ASSOCIATED_TOKEN => associated_token(data),
        // Only discriminant 0 has been seen; the logs name it "Process Report" / "Process Create".
        ORACLE_PROCESSOR => (u32_at(data, 0)? == 0).then_some(Decoded { kind: "report", transfer: None }),
        ORACLE_REGISTRY => (u32_at(data, 0)? == 0).then_some(Decoded { kind: "create", transfer: None }),
        _ => None,
    }
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn plain(kind: &'static str) -> Option<Decoded> {
    Some(Decoded { kind, transfer: None })
}

fn moves(kind: &'static str, source: usize, dest: usize, kelvin: Option<u64>) -> Option<Decoded> {
    Some(Decoded { kind, transfer: Some(Transfer { source, dest, mint: None, amount: kelvin? }) })
}

/// A token movement with a fixed-size layout: one discriminant byte, a u64 amount and, for the
/// `*_checked` variants, a decimals byte. Anything longer is not this instruction.
fn token_moves(kind: &'static str, d: &[u8], len: usize, source: usize, dest: usize, mint: usize) -> Option<Decoded> {
    if d.len() != len {
        return None;
    }
    Some(Decoded { kind, transfer: Some(Transfer { source, dest, mint: Some(mint), amount: u64_at(d, 1)? }) })
}

fn system(d: &[u8]) -> Option<Decoded> {
    match u32_at(d, 0)? {
        0 => moves("create_account", 0, 1, u64_at(d, 4)),
        1 => plain("assign"),
        2 => moves("transfer", 0, 1, u64_at(d, 4)),
        // base: Pubkey, seed: String (u64 length + bytes), lamports, space, owner
        3 => {
            let seed_len = usize::try_from(u64_at(d, 36)?).ok()?;
            moves("create_account_with_seed", 0, 1, u64_at(d, 44usize.checked_add(seed_len)?))
        }
        4 => plain("advance_nonce"),
        5 => moves("withdraw_nonce", 0, 1, u64_at(d, 4)),
        6 => plain("initialize_nonce"),
        7 => plain("authorize_nonce"),
        8 => plain("allocate"),
        9 => plain("allocate_with_seed"),
        10 => plain("assign_with_seed"),
        // accounts: from, base, to
        11 => moves("transfer_with_seed", 0, 2, u64_at(d, 4)),
        12 => plain("upgrade_nonce"),
        _ => None,
    }
}

/// SPL Token and Token-2022 share discriminants 0–24; 25 onwards are Token-2022 extensions.
/// Plain `transfer` (3) names no mint, so it gets a kind but no transfer row.
fn token(d: &[u8]) -> Option<Decoded> {
    let kind = match *d.first()? {
        // accounts: mint, destination, authority
        7 => return token_moves("mint_to", d, 9, 0, 1, 0),
        14 => return token_moves("mint_to_checked", d, 10, 0, 1, 0),
        // accounts: source, destination, owner
        3 => "transfer",
        // accounts: account, mint, owner
        8 => return token_moves("burn", d, 9, 0, 1, 1),
        15 => return token_moves("burn_checked", d, 10, 0, 1, 1),
        // accounts: source, mint, destination, owner
        12 => return token_moves("transfer_checked", d, 10, 0, 2, 1),
        0 => "initialize_mint",
        1 => "initialize_account",
        2 => "initialize_multisig",
        4 => "approve",
        5 => "revoke",
        6 => "set_authority",
        9 => "close_account",
        10 => "freeze_account",
        11 => "thaw_account",
        13 => "approve_checked",
        16 => "initialize_account2",
        17 => "sync_native",
        18 => "initialize_account3",
        19 => "initialize_multisig2",
        20 => "initialize_mint2",
        21 => "get_account_data_size",
        22 => "initialize_immutable_owner",
        23 => "amount_to_ui_amount",
        24 => "ui_amount_to_amount",
        25 => "initialize_mint_close_authority",
        26 => "transfer_fee_extension",
        27 => "confidential_transfer_extension",
        28 => "default_account_state_extension",
        29 => "reallocate",
        30 => "memo_transfer_extension",
        31 => "create_native_mint",
        32 => "initialize_non_transferable_mint",
        33 => "interest_bearing_mint_extension",
        34 => "cpi_guard_extension",
        35 => "initialize_permanent_delegate",
        36 => "transfer_hook_extension",
        37 => "confidential_transfer_fee_extension",
        38 => "withdraw_excess_lamports",
        39 => "metadata_pointer_extension",
        40 => "group_pointer_extension",
        41 => "group_member_pointer_extension",
        _ => return None,
    };
    plain(kind)
}

fn associated_token(d: &[u8]) -> Option<Decoded> {
    match d.first() {
        None | Some(0) => plain("create"),
        Some(1) => plain("create_idempotent"),
        Some(2) => plain("recover_nested"),
        _ => None,
    }
}

fn compute_budget(d: &[u8]) -> Option<Decoded> {
    let (kind, len) = match *d.first()? {
        1 => ("request_heap_frame", 5),
        2 => ("set_compute_unit_limit", 5),
        3 => ("set_compute_unit_price", 9),
        4 => ("set_loaded_accounts_data_size_limit", 5),
        _ => return None,
    };
    (d.len() == len).then_some(Decoded { kind, transfer: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b58(s: &str) -> Vec<u8> {
        bs58::decode(s).into_vec().unwrap()
    }

    // Instruction data copied from testnet blocks 17060537, 31410105 and 19602582.
    #[test]
    fn system_transfer() {
        let d = decode(SYSTEM, &b58("3Bxs3zzLZLuLQEYX")).unwrap();
        assert_eq!(
            d,
            Decoded {
                kind: "transfer",
                transfer: Some(Transfer { source: 0, dest: 1, mint: None, amount: 1_000_000_000 })
            }
        );
        let d = decode(SYSTEM, &b58("3Bxs412MvVNQj175")).unwrap();
        assert_eq!(d.transfer.unwrap().amount, 1);
    }

    #[test]
    fn system_create_account() {
        let d = decode(SYSTEM, &b58("111185bQPjKa3du8ita9oXw5x4QZEF8nyBiDtGj3Qk4kS4mEj8Mc7PGfMjGq8mitiMW8nj")).unwrap();
        assert_eq!(d.kind, "create_account");
        assert_eq!(d.transfer.unwrap().amount, 5_373_120);
    }

    #[test]
    fn compute_budget_ixs() {
        assert_eq!(decode(COMPUTE_BUDGET, &b58("HZoMm9")).unwrap().kind, "set_compute_unit_limit");
        assert_eq!(decode(COMPUTE_BUDGET, &b58("TB8Kiw")).unwrap().kind, "set_loaded_accounts_data_size_limit");
    }

    // Data shapes from testnet Token-2022 instructions whose logs read "MintTo" and "TransferChecked".
    #[test]
    fn token_movements() {
        let mut mint_to = vec![7];
        mint_to.extend(1_000_000u64.to_le_bytes());
        let d = decode(TOKEN_2022, &mint_to).unwrap();
        assert_eq!(
            d,
            Decoded {
                kind: "mint_to",
                transfer: Some(Transfer { source: 0, dest: 1, mint: Some(0), amount: 1_000_000 })
            }
        );

        let mut checked = vec![12];
        checked.extend(42u64.to_le_bytes());
        checked.push(6);
        let d = decode(TOKEN_2022, &checked).unwrap();
        assert_eq!(d.transfer, Some(Transfer { source: 0, dest: 2, mint: Some(1), amount: 42 }));

        assert_eq!(decode(TOKEN_2022, &[22]).unwrap(), Decoded { kind: "initialize_immutable_owner", transfer: None });
        assert_eq!(decode(ASSOCIATED_TOKEN, &[1]).unwrap().kind, "create_idempotent");
        assert_eq!(decode(ASSOCIATED_TOKEN, &[]).unwrap().kind, "create");
        // A longer payload behind byte 7 is some other instruction, not mint_to.
        assert_eq!(decode(TOKEN_2022, &[7; 20]), None);
    }

    #[test]
    fn oracle_kinds() {
        assert_eq!(decode(ORACLE_PROCESSOR, &[0, 0, 0, 0, 9, 9]).unwrap().kind, "report");
        assert_eq!(decode(ORACLE_REGISTRY, &[0, 0, 0, 0]).unwrap().kind, "create");
        assert_eq!(decode(ORACLE_REGISTRY, &[1, 0, 0, 0]), None);
    }

    #[test]
    fn truncated_data_is_not_decoded() {
        assert_eq!(decode(SYSTEM, &[2, 0, 0, 0, 1]), None);
        assert_eq!(decode(COMPUTE_BUDGET, &[2, 0]), None);
        assert_eq!(decode("Unknown1111111111111111111111111111111111111", &[2, 0, 0, 0]), None);
    }
}
