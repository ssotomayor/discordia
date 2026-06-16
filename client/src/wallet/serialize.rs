//! Solana's wire format — just enough of it to serialize a legacy
//! `Message` and `Transaction` for a System Program transfer.
//!
//! Two quirks to know:
//!
//! 1. **compact-u16** ("shortvec") — vector lengths are encoded as a
//!    variable-length integer: 1 byte if the value fits in 7 bits, 2 bytes
//!    if it fits in 14 bits, 3 bytes otherwise. Each byte contributes 7
//!    bits low-to-high, with the high bit signaling "more bytes follow."
//!    For our transactions (≤3 accounts, tiny instruction data) we always
//!    land in the 1-byte range — but the encoder handles the general case
//!    so we don't have to remember the rule.
//!
//! 2. **Account ordering** — `account_keys` must list signers before
//!    non-signers, and writable before readonly inside each group. For a
//!    `System.Transfer(from → to)` that means `[from, to, system_program]`.

/// Append the compact-u16 (shortvec) encoding of `n` to `out`.
pub fn write_shortvec(out: &mut Vec<u8>, mut n: u16) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        b |= 0x80;
        out.push(b);
    }
}

/// One legacy Solana transaction message. Layout matches what
/// `sendTransaction` expects when wrapped in the Transaction envelope.
pub struct Message<'a> {
    /// 1 byte each: required_signatures, readonly_signed, readonly_unsigned.
    pub header: [u8; 3],
    /// Pubkeys in canonical order (signers writable, signers readonly,
    /// non-signers writable, non-signers readonly).
    pub account_keys: &'a [[u8; 32]],
    pub recent_blockhash: [u8; 32],
    pub instructions: &'a [CompiledInstruction<'a>],
}

pub struct CompiledInstruction<'a> {
    pub program_id_index: u8,
    /// Each entry is an index into `account_keys`.
    pub accounts: &'a [u8],
    pub data: &'a [u8],
}

impl<'a> Message<'a> {
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.account_keys.len() * 32);
        out.extend_from_slice(&self.header);

        write_shortvec(&mut out, self.account_keys.len() as u16);
        for k in self.account_keys {
            out.extend_from_slice(k);
        }

        out.extend_from_slice(&self.recent_blockhash);

        write_shortvec(&mut out, self.instructions.len() as u16);
        for ix in self.instructions {
            out.push(ix.program_id_index);
            write_shortvec(&mut out, ix.accounts.len() as u16);
            out.extend_from_slice(ix.accounts);
            write_shortvec(&mut out, ix.data.len() as u16);
            out.extend_from_slice(ix.data);
        }
        out
    }
}

/// Wrap the signed message bytes with the signature(s) into the on-wire
/// transaction encoding.
pub fn serialize_transaction(signatures: &[[u8; 64]], message_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(signatures.len() * 64 + message_bytes.len() + 4);
    write_shortvec(&mut out, signatures.len() as u16);
    for s in signatures {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(message_bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortvec_one_byte() {
        let mut buf = Vec::new();
        write_shortvec(&mut buf, 0);
        assert_eq!(buf, [0]);

        buf.clear();
        write_shortvec(&mut buf, 1);
        assert_eq!(buf, [1]);

        buf.clear();
        write_shortvec(&mut buf, 127);
        assert_eq!(buf, [127]);
    }

    #[test]
    fn shortvec_two_byte_boundary() {
        let mut buf = Vec::new();
        write_shortvec(&mut buf, 128);
        // 128 = 0x80 — needs 2 bytes: [0x80, 0x01]
        assert_eq!(buf, [0x80, 0x01]);
    }

    #[test]
    fn message_serialize_matches_known_layout() {
        // Minimal transfer: from=0x01..01, to=0x02..02, system=0x00..00.
        // Tests the byte-by-byte layout against what a Solana validator
        // would expect, so regressions in the encoder show up here.
        let from = [1u8; 32];
        let to = [2u8; 32];
        let sys = [0u8; 32];
        let blockhash = [9u8; 32];
        let data = {
            let mut d = vec![2, 0, 0, 0]; // discriminator
            d.extend_from_slice(&1_000_000u64.to_le_bytes());
            d
        };
        let ix = CompiledInstruction {
            program_id_index: 2,
            accounts: &[0, 1],
            data: &data,
        };
        let msg = Message {
            header: [1, 0, 1],
            account_keys: &[from, to, sys],
            recent_blockhash: blockhash,
            instructions: &[ix],
        };
        let bytes = msg.serialize();

        let mut expected = Vec::new();
        expected.extend_from_slice(&[1, 0, 1]); // header
        expected.push(3); // account_keys length
        expected.extend_from_slice(&from);
        expected.extend_from_slice(&to);
        expected.extend_from_slice(&sys);
        expected.extend_from_slice(&blockhash);
        expected.push(1); // instructions length
        expected.push(2); // program_id_index
        expected.push(2); // accounts length
        expected.extend_from_slice(&[0, 1]);
        expected.push(12); // data length
        expected.extend_from_slice(&data);

        assert_eq!(bytes, expected);
    }
}
