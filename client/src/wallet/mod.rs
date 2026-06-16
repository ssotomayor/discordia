//! Solana wallet — RPC reads (balance, blockhash) plus SOL transfer
//! construction + signing using the user's identity keypair.
//!
//! Scoped intentionally small for v1: SOL only, no SPL tokens, no
//! transaction history. The crypto primitives we already use for identity
//! (ed25519-dalek, bs58) double as the wallet primitives — a wallet *is*
//! the identity, just spent in a different context.

pub mod rpc;
mod serialize;
pub mod spl;
pub mod tx;

pub use rpc::{Network, RpcClient, TokenHolding, TxRecord};
pub use tx::{send_sol, send_spl_token};

/// Multiply a UI-level token amount (e.g. "0.5") by 10^decimals to get the
/// raw on-chain unit, with overflow + sign + NaN checks.
pub fn ui_amount_to_raw(ui_amount: f64, decimals: u8) -> Option<u64> {
    if !ui_amount.is_finite() || ui_amount < 0.0 {
        return None;
    }
    let scale = 10f64.powi(decimals as i32);
    let raw = (ui_amount * scale).round();
    if raw > u64::MAX as f64 {
        return None;
    }
    Some(raw as u64)
}

/// Convert SOL (human-readable) → lamports (on-chain unit). 1 SOL = 10^9
/// lamports.
pub fn sol_to_lamports(sol: f64) -> Option<u64> {
    if !sol.is_finite() || sol < 0.0 {
        return None;
    }
    let lamports = (sol * 1_000_000_000.0).round();
    if lamports > u64::MAX as f64 {
        return None;
    }
    Some(lamports as u64)
}

/// Format lamports as a human-readable SOL string with up to 9 decimals
/// (trailing zeros trimmed).
pub fn lamports_to_sol_display(lamports: u64) -> String {
    let whole = lamports / 1_000_000_000;
    let frac = lamports % 1_000_000_000;
    if frac == 0 {
        return format!("{whole}");
    }
    let s = format!("{whole}.{:09}", frac);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sol_lamports_round_trip() {
        assert_eq!(sol_to_lamports(1.0), Some(1_000_000_000));
        assert_eq!(sol_to_lamports(0.5), Some(500_000_000));
        assert_eq!(sol_to_lamports(0.000_000_001), Some(1));
        assert_eq!(sol_to_lamports(0.0), Some(0));
        assert_eq!(sol_to_lamports(-1.0), None);
        assert_eq!(sol_to_lamports(f64::NAN), None);
    }

    #[test]
    fn ui_to_raw_scales_by_decimals() {
        assert_eq!(ui_amount_to_raw(1.0, 6), Some(1_000_000));
        assert_eq!(ui_amount_to_raw(0.5, 6), Some(500_000));
        assert_eq!(ui_amount_to_raw(1.5, 9), Some(1_500_000_000));
        assert_eq!(ui_amount_to_raw(0.0, 6), Some(0));
        assert_eq!(ui_amount_to_raw(-1.0, 6), None);
        assert_eq!(ui_amount_to_raw(f64::NAN, 6), None);
    }

    #[test]
    fn lamports_display_trims() {
        assert_eq!(lamports_to_sol_display(0), "0");
        assert_eq!(lamports_to_sol_display(1_000_000_000), "1");
        assert_eq!(lamports_to_sol_display(1_500_000_000), "1.5");
        assert_eq!(lamports_to_sol_display(1), "0.000000001");
        assert_eq!(lamports_to_sol_display(123_456_789), "0.123456789");
    }
}
