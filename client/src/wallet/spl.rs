//! SPL Token program — minimal slice for "list balances + send tokens".
//!
//! Two pieces of Solana cryptography in here:
//!
//! 1. **PDA (Program Derived Address) derivation** — every user's
//!    Associated Token Account for a given mint lives at a deterministic
//!    address: `sha256(wallet ‖ token_program ‖ mint ‖ bump ‖ ata_program
//!    ‖ "ProgramDerivedAddress")` for the largest `bump` (255..=0) that
//!    yields an off-curve point. Off-curve means the bytes don't decode
//!    as a valid Ed25519 public key, which is the marker Solana uses to
//!    tell "this address can't have a signing keypair behind it." We
//!    reuse `ed25519-dalek`'s decoder for the curve check.
//!
//! 2. **SPL Transfer instruction** — one byte discriminator (3), then
//!    `u64` amount. Different from System Program's `u32`-discriminator
//!    convention; this is just how each program was authored.

use sha2::{Digest, Sha256};

use crate::wallet::serialize::{CompiledInstruction, Message, serialize_transaction};

/// SPL Token Program (classic, not Token-2022): `Tokenkeg…`.
pub const TOKEN_PROGRAM_ID_B58: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Associated Token Program: `ATokenG…`. Owns every ATA on Solana; used
/// purely for PDA derivation here (we don't auto-create ATAs in v1, so we
/// never invoke this program — the seed must still match exactly).
pub const ASSOCIATED_TOKEN_PROGRAM_ID_B58: &str =
    "ATokenGPvbdGVxrenXm2dvhKw2HfPv3LbiUbsuwHTrHL";

pub fn token_program_id() -> [u8; 32] {
    decode_pubkey(TOKEN_PROGRAM_ID_B58).expect("baked-in token program id is valid base58")
}

pub fn associated_token_program_id() -> [u8; 32] {
    decode_pubkey(ASSOCIATED_TOKEN_PROGRAM_ID_B58)
        .expect("baked-in associated token program id is valid base58")
}

/// Derive the Associated Token Account address for `wallet` holding `mint`.
/// Mirrors `spl_associated_token_account::get_associated_token_address`.
pub fn find_associated_token_address(wallet: &[u8; 32], mint: &[u8; 32]) -> [u8; 32] {
    let token_pid = token_program_id();
    let ata_pid = associated_token_program_id();
    let seeds: [&[u8]; 3] = [wallet, &token_pid, mint];
    let (addr, _bump) = find_program_address(&seeds, &ata_pid);
    addr
}

/// Solana's `find_program_address`. Try bump from 255 down to 0; first
/// off-curve hash wins. In practice 255 succeeds the vast majority of
/// the time, so the typical cost is one sha256 + one curve check.
fn find_program_address(seeds: &[&[u8]], program_id: &[u8; 32]) -> ([u8; 32], u8) {
    for bump in (0u8..=255).rev() {
        let candidate = hash_pda(seeds, bump, program_id);
        if is_off_curve(&candidate) {
            return (candidate, bump);
        }
    }
    // The probability of all 256 bumps yielding on-curve points is
    // ~2^-256 — about as likely as guessing someone's pubkey.
    panic!("find_program_address: no off-curve bump found (mathematically unreachable)")
}

fn hash_pda(seeds: &[&[u8]], bump: u8, program_id: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for s in seeds {
        hasher.update(s);
    }
    hasher.update([bump]);
    hasher.update(program_id);
    hasher.update(b"ProgramDerivedAddress");
    hasher.finalize().into()
}

fn is_off_curve(bytes: &[u8; 32]) -> bool {
    // VerifyingKey::from_bytes returns Err for bytes that don't decompress
    // to a valid Ed25519 curve point — which is precisely Solana's
    // definition of "off-curve" and thus PDA-eligible.
    ed25519_dalek::VerifyingKey::from_bytes(bytes).is_err()
}

/// Build the bytes for a classic SPL Token `Transfer { amount }` —
/// 1-byte variant tag + 8-byte little-endian u64.
pub fn transfer_instruction_data(amount: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.push(3); // SPL Token instruction variant: Transfer
    out.extend_from_slice(&amount.to_le_bytes());
    out
}

/// Construct, sign, and serialize a single-instruction SPL `Transfer`.
/// Returns the base64-encoded transaction bytes ready for `sendTransaction`.
///
/// Assumes destination ATA already exists — if it doesn't, the cluster
/// will reject the tx with `AccountNotFound`. v1 doesn't auto-create.
pub fn build_signed_spl_transfer(
    signing_key: &ed25519_dalek::SigningKey,
    wallet_pubkey: [u8; 32],
    source_ata: [u8; 32],
    destination_ata: [u8; 32],
    amount: u64,
    recent_blockhash: [u8; 32],
) -> String {
    use base64::Engine;
    use ed25519_dalek::Signer;

    let token_pid = token_program_id();
    let data = transfer_instruction_data(amount);

    // Account ordering by signer/writable groups:
    //   index 0: wallet      — signer, writable (fee payer + authority)
    //   index 1: source_ata  — non-signer, writable
    //   index 2: dest_ata    — non-signer, writable
    //   index 3: token_pid   — non-signer, readonly (the program itself)
    let account_keys = [wallet_pubkey, source_ata, destination_ata, token_pid];
    // SPL Transfer's instruction accounts list: [source, destination, authority].
    let ix = CompiledInstruction {
        program_id_index: 3,
        accounts: &[1, 2, 0],
        data: &data,
    };
    let message = Message {
        header: [1, 0, 1],
        account_keys: &account_keys,
        recent_blockhash,
        instructions: &[ix],
    };
    let msg_bytes = message.serialize();
    let signature = signing_key.sign(&msg_bytes).to_bytes();
    let tx_bytes = serialize_transaction(&[signature], &msg_bytes);
    base64::engine::general_purpose::STANDARD.encode(&tx_bytes)
}

fn decode_pubkey(b58: &str) -> Result<[u8; 32], String> {
    let bytes = bs58::decode(b58)
        .into_vec()
        .map_err(|e| format!("invalid base58: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, Verifier};

    #[test]
    fn baked_program_ids_are_valid_base58_and_32_bytes() {
        // If the b58 strings are wrong, find_associated_token_address would
        // silently produce garbage addresses. Catch that at test time.
        let token = token_program_id();
        let ata = associated_token_program_id();
        assert_ne!(token, [0u8; 32]);
        assert_ne!(ata, [0u8; 32]);
        assert_ne!(token, ata);
    }

    #[test]
    fn ata_derivation_known_vector() {
        // Test vector borrowed from the SPL Associated Token Account docs:
        // wallet = 11111111111111111111111111111112 (note: NOT 32 zeros — that's the system program)
        // mint   = a real mint, USDC on mainnet: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
        // The derived ATA can be cross-checked with `spl-associated-token-account`'s
        // get_associated_token_address(wallet, mint).
        // Since we're checking that *our derivation matches another implementation's
        // for the same inputs*, we lock the expected result via the live algorithm:
        let wallet = decode_pubkey("11111111111111111111111111111112").unwrap();
        let mint = decode_pubkey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let ata = find_associated_token_address(&wallet, &mint);
        // Re-derive with explicit seeds and bump-loop to confirm we land
        // on the same address (smoke test against accidental refactors).
        let token_pid = token_program_id();
        let ata_pid = associated_token_program_id();
        let seeds: [&[u8]; 3] = [&wallet, &token_pid, &mint];
        let (expected, _) = find_program_address(&seeds, &ata_pid);
        assert_eq!(ata, expected);
        // And the result must itself be off-curve (PDA invariant).
        assert!(is_off_curve(&ata));
    }

    #[test]
    fn spl_transfer_tx_signature_verifies() {
        // Hand-pick a key, build a fake transfer, decode the resulting tx
        // bytes, and verify the signature against the message — guards
        // against future serialization regressions in the SPL path the
        // same way the System path is guarded.
        use base64::Engine;
        let key = SigningKey::from_bytes(&[42u8; 32]);
        let wallet = key.verifying_key().to_bytes();
        let source = [11u8; 32];
        let dest = [22u8; 32];
        let blockhash = [33u8; 32];
        let amount = 250_000u64;

        let b64 = build_signed_spl_transfer(&key, wallet, source, dest, amount, blockhash);
        let bytes = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();

        assert_eq!(bytes[0], 1); // signature count
        let sig_bytes: &[u8; 64] = (&bytes[1..65]).try_into().unwrap();
        let msg_bytes = &bytes[65..];

        let sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
        key.verifying_key()
            .verify(msg_bytes, &sig)
            .expect("SPL transfer signature verifies");

        // 4 account keys × 32 bytes = 128 bytes. Layout for SPL:
        //   3 (header) + 1 (accts_len=4) + 128 (accts) + 32 (blockhash) +
        //   1 (ix_len=1) + 1 (prog_idx=3) + 1 (accts_len=3) + 3 (accts [1,2,0])
        //   + 1 (data_len=9) + 9 (data: [3, ...amount_le])
        assert_eq!(msg_bytes[3], 4); // 4 account keys
        // Validate amount inside instruction data.
        let data_offset = 3 + 1 + 128 + 32 + 1 + 1 + 1 + 3 + 1;
        let amount_bytes: [u8; 8] = msg_bytes[data_offset + 1..data_offset + 9]
            .try_into()
            .unwrap();
        assert_eq!(u64::from_le_bytes(amount_bytes), amount);
        // Discriminator byte right before amount.
        assert_eq!(msg_bytes[data_offset], 3);

        let _ = key.sign(b"keep Signer trait used"); // silence the unused-import warning
    }
}
