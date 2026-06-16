//! Build, sign, and submit a SOL transfer.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

use crate::wallet::rpc::RpcClient;
use crate::wallet::serialize::{CompiledInstruction, Message, serialize_transaction};

/// All-zero pubkey of the System Program (`11111111111111111111111111111111`).
const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// Construct a `System.Transfer(from → to, lamports)`, sign it with the
/// caller's signing key, submit it to the RPC, and return the resulting
/// transaction signature.
///
/// "Submitted" means accepted by the validator. The signature may still
/// fail to land if the blockhash expires before the cluster confirms — the
/// caller should treat the returned signature as a *receipt to look up
/// later* (via the explorer URL helper), not as proof of finality.
pub async fn send_sol(
    rpc: &RpcClient,
    signing_key: &SigningKey,
    from_pubkey_b58: &str,
    to_pubkey_b58: &str,
    lamports: u64,
) -> Result<String, String> {
    let from = decode_pubkey(from_pubkey_b58).map_err(|e| format!("from pubkey: {e}"))?;
    let to = decode_pubkey(to_pubkey_b58).map_err(|e| format!("to pubkey: {e}"))?;
    validate_transfer(signing_key, &from, &to, lamports)?;

    let blockhash_b58 = rpc.get_latest_blockhash().await?;
    let recent_blockhash = decode_pubkey(&blockhash_b58)
        .map_err(|e| format!("blockhash: {e}"))?;

    let tx_b64 = build_signed_transfer(signing_key, from, to, lamports, recent_blockhash);

    rpc.send_transaction(&tx_b64).await
}

/// Pre-flight checks. Pulled out so they're testable without an RPC.
fn validate_transfer(
    signing_key: &SigningKey,
    from: &[u8; 32],
    to: &[u8; 32],
    lamports: u64,
) -> Result<(), String> {
    if from == to {
        return Err("cannot send to yourself".to_string());
    }
    if lamports == 0 {
        return Err("amount must be > 0".to_string());
    }
    // Sanity check: the signing key actually produces this from-pubkey.
    // Catches `Identity` / wallet mismatches before we spend a blockhash.
    let signing_pubkey = signing_key.verifying_key().to_bytes();
    if &signing_pubkey != from {
        return Err("signing key does not match from pubkey".to_string());
    }
    Ok(())
}

fn build_signed_transfer(
    signing_key: &SigningKey,
    from: [u8; 32],
    to: [u8; 32],
    lamports: u64,
    recent_blockhash: [u8; 32],
) -> String {
    // Transfer instruction: 4-byte LE u32 discriminator (2) + 8-byte LE u64.
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());

    let account_keys = [from, to, SYSTEM_PROGRAM_ID];
    let ix = CompiledInstruction {
        program_id_index: 2,
        accounts: &[0, 1],
        data: &data,
    };
    let message = Message {
        // 1 required signature (from), 0 readonly signed, 1 readonly
        // unsigned (the system program).
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
    let bytes = bs58::decode(b58.trim())
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
    use ed25519_dalek::Verifier;

    fn fixed_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn validate_rejects_zero_amount() {
        let key = fixed_key();
        let from = key.verifying_key().to_bytes();
        let to = [1u8; 32];
        let err = validate_transfer(&key, &from, &to, 0).unwrap_err();
        assert!(err.contains("must be > 0"));
    }

    #[test]
    fn validate_rejects_self_transfer() {
        let key = fixed_key();
        let from = key.verifying_key().to_bytes();
        let err = validate_transfer(&key, &from, &from, 1000).unwrap_err();
        assert!(err.contains("cannot send to yourself"));
    }

    #[test]
    fn validate_rejects_mismatched_signing_key() {
        let key = fixed_key();
        let from = [9u8; 32]; // not the verifying key
        let to = [1u8; 32];
        let err = validate_transfer(&key, &from, &to, 1000).unwrap_err();
        assert!(err.contains("signing key does not match"));
    }

    #[test]
    fn validate_passes_happy_path() {
        let key = fixed_key();
        let from = key.verifying_key().to_bytes();
        let to = [1u8; 32];
        assert!(validate_transfer(&key, &from, &to, 1000).is_ok());
    }

    #[test]
    fn build_signed_tx_signature_verifies() {
        // Reconstruct the message and verify the ed25519 signature in the
        // tx bytes lines up with from-pubkey. This catches signing
        // regressions without needing a live validator.
        let key = fixed_key();
        let from = key.verifying_key().to_bytes();
        let to = [2u8; 32];
        let blockhash = [9u8; 32];
        let lamports = 1_500_000u64;

        let b64 = build_signed_transfer(&key, from, to, lamports, blockhash);
        let bytes = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();

        // Layout: [sig_count_shortvec=1][sig=64 bytes][message]
        assert_eq!(bytes[0], 1);
        let sig_bytes: &[u8; 64] = (&bytes[1..65]).try_into().unwrap();
        let msg_bytes = &bytes[65..];

        let sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
        key.verifying_key().verify(msg_bytes, &sig).expect("signature verifies");

        // Spot-check the message layout. Offsets:
        //   0..3      header
        //   3         account_keys length (= 3)
        //   4..36     from
        //   36..68    to
        //   68..100   system program
        //   100..132  blockhash
        //   132       instructions length (= 1)
        //   133       program_id_index (= 2)
        //   134       accounts length (= 2)
        //   135..137  accounts [0, 1]
        //   137       data length (= 12)
        //   138..142  Transfer discriminator [2,0,0,0]
        //   142..150  lamports LE
        assert_eq!(&msg_bytes[..3], &[1, 0, 1]);
        assert_eq!(msg_bytes[3], 3);
        assert_eq!(&msg_bytes[4..36], &from);
        assert_eq!(&msg_bytes[36..68], &to);
        assert_eq!(&msg_bytes[68..100], &SYSTEM_PROGRAM_ID);
        assert_eq!(&msg_bytes[100..132], &blockhash);
        assert_eq!(&msg_bytes[138..142], &[2, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(msg_bytes[142..150].try_into().unwrap()), lamports);
    }
}
