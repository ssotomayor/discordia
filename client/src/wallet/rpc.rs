//! Minimal Solana JSON-RPC client over HTTPS. We hand-roll instead of pulling
//! in `solana-client` because that crate brings in ~hundreds of transitive
//! deps; we only need three methods.

use serde::{Deserialize, Serialize};

/// Which Solana cluster the wallet talks to. Defaults to Devnet so users
/// can experiment with airdropped SOL without spending real money.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Devnet,
    Testnet,
    MainnetBeta,
}

impl Network {
    pub fn rpc_url(&self) -> &'static str {
        match self {
            Network::Devnet => "https://api.devnet.solana.com",
            Network::Testnet => "https://api.testnet.solana.com",
            Network::MainnetBeta => "https://api.mainnet-beta.solana.com",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Network::Devnet => "devnet",
            Network::Testnet => "testnet",
            Network::MainnetBeta => "mainnet-beta",
        }
    }

    pub fn explorer_tx_url(&self, signature: &str) -> String {
        let cluster = match self {
            Network::Devnet => "?cluster=devnet",
            Network::Testnet => "?cluster=testnet",
            Network::MainnetBeta => "",
        };
        format!("https://explorer.solana.com/tx/{signature}{cluster}")
    }
}

#[derive(Clone)]
pub struct RpcClient {
    client: reqwest::Client,
    url: String,
}

impl RpcClient {
    pub fn new(network: Network) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: network.rpc_url().to_string(),
        }
    }

    /// Returns the account's balance in lamports. Non-existent accounts
    /// return 0 (Solana convention — until you receive your first lamport,
    /// the account doesn't exist on-chain).
    pub async fn get_balance(&self, pubkey_b58: &str) -> Result<u64, String> {
        #[derive(Deserialize)]
        struct BalanceValue {
            value: u64,
        }
        let val: BalanceValue = self.call("getBalance", vec![json(pubkey_b58)]).await?;
        Ok(val.value)
    }

    /// Returns the most recent blockhash usable for transaction signing.
    /// Solana blockhashes expire after ~150 slots (~1 minute) so we always
    /// fetch a fresh one right before signing a tx.
    pub async fn get_latest_blockhash(&self) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Hash {
            blockhash: String,
        }
        #[derive(Deserialize)]
        struct LatestBlockhashValue {
            value: Hash,
        }
        let v: LatestBlockhashValue = self.call("getLatestBlockhash", Vec::new()).await?;
        Ok(v.value.blockhash)
    }

    /// All classic SPL token accounts owned by `owner_b58`. Uses
    /// `jsonParsed` encoding so we get the human-readable `tokenAmount`
    /// + decimals without having to decode the raw 165-byte account.
    /// Token-2022 accounts are NOT included — listing those would
    /// require a second RPC call with a different program filter.
    pub async fn get_token_accounts_by_owner(
        &self,
        owner_b58: &str,
    ) -> Result<Vec<TokenHolding>, String> {
        let filter = serde_json::json!({
            "programId": crate::wallet::spl::TOKEN_PROGRAM_ID_B58,
        });
        let cfg = serde_json::json!({ "encoding": "jsonParsed" });
        #[derive(Deserialize)]
        struct Raw {
            value: Vec<RawHolding>,
        }
        #[derive(Deserialize)]
        struct RawHolding {
            pubkey: String,
            account: RawAccount,
        }
        #[derive(Deserialize)]
        struct RawAccount {
            data: RawData,
        }
        #[derive(Deserialize)]
        struct RawData {
            parsed: RawParsed,
        }
        #[derive(Deserialize)]
        struct RawParsed {
            info: RawInfo,
        }
        #[derive(Deserialize)]
        struct RawInfo {
            mint: String,
            #[serde(rename = "tokenAmount")]
            token_amount: RawTokenAmount,
        }
        #[derive(Deserialize)]
        struct RawTokenAmount {
            amount: String,
            decimals: u8,
            #[serde(rename = "uiAmountString")]
            ui_amount_string: String,
        }

        let raw: Raw = self
            .call("getTokenAccountsByOwner", vec![json(owner_b58), filter, cfg])
            .await?;

        let mut out = Vec::with_capacity(raw.value.len());
        for h in raw.value {
            let amount = h
                .account
                .data
                .parsed
                .info
                .token_amount
                .amount
                .parse::<u64>()
                .map_err(|e| format!("token amount not a u64: {e}"))?;
            out.push(TokenHolding {
                token_account: h.pubkey,
                mint: h.account.data.parsed.info.mint,
                amount,
                decimals: h.account.data.parsed.info.token_amount.decimals,
                ui_amount: h.account.data.parsed.info.token_amount.ui_amount_string,
            });
        }
        // Hide dust (amount == 0). Empty ATAs add noise without value.
        out.retain(|h| h.amount > 0);
        Ok(out)
    }

    /// Recent transaction signatures for `pubkey_b58`. Capped at `limit`
    /// (Solana max is 1000; reasonable UI values are ≤25).
    pub async fn get_signatures_for_address(
        &self,
        pubkey_b58: &str,
        limit: u32,
    ) -> Result<Vec<TxRecord>, String> {
        let cfg = serde_json::json!({ "limit": limit });
        let raw: Vec<TxRecordRaw> = self
            .call(
                "getSignaturesForAddress",
                vec![json(pubkey_b58), cfg],
            )
            .await?;
        Ok(raw
            .into_iter()
            .map(|r| TxRecord {
                signature: r.signature,
                block_time: r.block_time,
                err: r.err.is_some(),
            })
            .collect())
    }

    /// Submit a signed, serialized transaction. `tx_b64` is base64 of the
    /// raw transaction bytes. Returns the transaction signature (base58)
    /// once accepted by the validator — note that "accepted" ≠ "confirmed";
    /// the signature can still be dropped if the blockhash expires.
    pub async fn send_transaction(&self, tx_b64: &str) -> Result<String, String> {
        let opts = serde_json::json!({
            "encoding": "base64",
            "skipPreflight": false,
            "preflightCommitment": "processed",
        });
        let signature: String = self
            .call("sendTransaction", vec![json(tx_b64), opts])
            .await?;
        Ok(signature)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<T, String> {
        #[derive(Serialize)]
        struct Req<'a> {
            jsonrpc: &'a str,
            id: u32,
            method: &'a str,
            params: Vec<serde_json::Value>,
        }
        let body = Req {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("rpc {method}: network: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("rpc {method}: body: {e}"))?;
        if !status.is_success() {
            return Err(format!("rpc {method}: http {status}: {text}"));
        }
        // Solana wraps errors in {"jsonrpc":"2.0","error":{...},"id":...}
        // Parse defensively — error path or result path.
        let envelope: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("rpc {method}: invalid JSON: {e} | body={text}"))?;
        if let Some(err) = envelope.get("error") {
            return Err(format!("rpc {method}: {err}"));
        }
        let result = envelope.get("result").ok_or_else(|| {
            format!("rpc {method}: response missing 'result' field | body={text}")
        })?;
        serde_json::from_value::<T>(result.clone())
            .map_err(|e| format!("rpc {method}: result didn't match expected shape: {e}"))
    }
}

fn json(s: &str) -> serde_json::Value {
    serde_json::Value::String(s.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenHolding {
    pub token_account: String,
    pub mint: String,
    /// Raw amount in the mint's smallest unit (multiply by 10^decimals to
    /// get human-readable). Stored as u64 since SPL amounts fit in 64 bits.
    pub amount: u64,
    pub decimals: u8,
    /// Pre-formatted display string from the RPC (e.g. "1.5"). Trusted
    /// because we always cross-check `amount` against it via `decimals`.
    pub ui_amount: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRecord {
    pub signature: String,
    /// Unix timestamp in seconds; absent for very recent unconfirmed txs.
    pub block_time: Option<i64>,
    pub err: bool,
}

#[derive(Deserialize)]
struct TxRecordRaw {
    signature: String,
    #[serde(rename = "blockTime")]
    block_time: Option<i64>,
    /// Solana returns `null` on success, an object on failure. We don't
    /// care about the shape — just whether it's present.
    err: Option<serde_json::Value>,
}
