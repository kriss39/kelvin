//! Labels and decoders for Rialo's native programs. Layouts are Solana's (little-endian; System
//! uses a u32 discriminant, ComputeBudget a u8), checked against testnet blocks on 2026-10-11.

pub const SYSTEM: &str = "11111111111111111111111111111111";
pub const COMPUTE_BUDGET: &str = "ComputeBudget111111111111111111111111111111";
/// Every block carries one transaction calling only this program ("FreezeStakes").
pub const TOKENOMICS: &str = "TokenomicsGovernance11111111111111111111111";
pub const ORACLE_PROCESSOR: &str = "Qrac1eProcessor1111111111111111111111111111";
pub const ORACLE_REGISTRY: &str = "Qrac1eRegistry11111111111111111111111111111";
pub const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

pub fn label(program: &str) -> Option<&'static str> {
    Some(match program {
        SYSTEM => "System",
        COMPUTE_BUDGET => "ComputeBudget",
        TOKENOMICS => "TokenomicsGovernance",
        ORACLE_PROCESSOR => "OracleProcessor",
        ORACLE_REGISTRY => "OracleRegistry",
        TOKEN_2022 => "Token2022",
        ASSOCIATED_TOKEN => "AssociatedToken",
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

/// Native RLO movement. `source` and `dest` are positions in the instruction's account list.
#[derive(Debug, PartialEq)]
pub struct Transfer {
    pub source: usize,
    pub dest: usize,
    pub kelvin: u64,
}

pub fn decode(program: &str, data: &[u8]) -> Option<Decoded> {
    match program {
        SYSTEM => system(data),
        COMPUTE_BUDGET => compute_budget(data),
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
    Some(Decoded { kind, transfer: Some(Transfer { source, dest, kelvin: kelvin? }) })
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
        assert_eq!(d, Decoded { kind: "transfer", transfer: Some(Transfer { source: 0, dest: 1, kelvin: 1_000_000_000 }) });
        let d = decode(SYSTEM, &b58("3Bxs412MvVNQj175")).unwrap();
        assert_eq!(d.transfer.unwrap().kelvin, 1);
    }

    #[test]
    fn system_create_account() {
        let d = decode(SYSTEM, &b58("111185bQPjKa3du8ita9oXw5x4QZEF8nyBiDtGj3Qk4kS4mEj8Mc7PGfMjGq8mitiMW8nj")).unwrap();
        assert_eq!(d.kind, "create_account");
        assert_eq!(d.transfer.unwrap().kelvin, 5_373_120);
    }

    #[test]
    fn compute_budget_ixs() {
        assert_eq!(decode(COMPUTE_BUDGET, &b58("HZoMm9")).unwrap().kind, "set_compute_unit_limit");
        assert_eq!(decode(COMPUTE_BUDGET, &b58("TB8Kiw")).unwrap().kind, "set_loaded_accounts_data_size_limit");
    }

    #[test]
    fn truncated_data_is_not_decoded() {
        assert_eq!(decode(SYSTEM, &[2, 0, 0, 0, 1]), None);
        assert_eq!(decode(COMPUTE_BUDGET, &[2, 0]), None);
        assert_eq!(decode("Unknown1111111111111111111111111111111111111", &[2, 0, 0, 0]), None);
    }
}
