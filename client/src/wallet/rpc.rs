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
