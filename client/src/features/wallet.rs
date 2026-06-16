//! In-app Solana wallet — receive (show address), send (SOL or SPL token),
//! and activity (recent transactions). Network selector lets users switch
//! between devnet (default, safe to experiment), testnet, and mainnet-beta.

use std::sync::Arc;

use dioxus::prelude::*;

use crate::identity::Identity;
use crate::wallet::{
    Network, RpcClient, TokenHolding, TxRecord, lamports_to_sol_display, send_sol, send_spl_token,
    sol_to_lamports, ui_amount_to_raw,
};

/// Wallet button + drawer in one. Mount inside the workspace; takes only
/// the identity so it has the pubkey + signing key it needs.
#[component]
pub fn WalletControls(identity: Identity) -> Element {
    let mut open = use_signal(|| false);

    rsx! {
        button {
            class: "shrink-0 px-3 py-2 bg-[var(--panel)] border border-[var(--border)] hover:border-[var(--accent)] rounded-lg text-xs flex items-center gap-2 panel-hover transition-colors",
            title: "Open wallet",
            onclick: move |_| open.set(true),
            svg {
                width: "14", height: "14", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "2",
                stroke_linecap: "round", stroke_linejoin: "round",
                class: "text-[var(--accent)]",
                rect { x: "2", y: "6", width: "20", height: "14", rx: "2" }
                path { d: "M16 14h.01" }
                path { d: "M2 10h20" }
            }
            span { class: "text-[var(--text-muted)] uppercase tracking-wider text-[10px] font-medium",
                "wallet"
            }
        }

        if open() {
            WalletDrawer {
                identity: identity.clone(),
                on_close: move |_| open.set(false),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Receive,
    Send,
    Activity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Sending,
    Sent { signature: String, explorer_url: String },
    Failed(String),
}

/// What's selected in the Send tab's token picker. `None` = native SOL.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SendAsset {
    Sol,
    Token(TokenHolding),
}

#[component]
fn WalletDrawer(identity: Identity, on_close: EventHandler<()>) -> Element {
    let mut tab = use_signal(|| Tab::Receive);
    let mut network = use_signal(|| Network::Devnet);
    let mut balance = use_signal(|| None::<u64>);
    let mut balance_error = use_signal(|| None::<String>);
    let mut tokens = use_signal(|| Vec::<TokenHolding>::new());
    let mut tokens_error = use_signal(|| None::<String>);
    let mut history = use_signal(|| Vec::<TxRecord>::new());
    let mut history_error = use_signal(|| None::<String>);
    let mut copied = use_signal(|| false);
    let recipient = use_signal(String::new);
    let amount = use_signal(String::new);
    let send_asset = use_signal(|| SendAsset::Sol);
    let status = use_signal(|| Status::Idle);

    // Refetch whenever the network changes (this also fires on first
    // render). Resets the per-tab payloads to "loading" so the UI doesn't
    // display stale devnet data after switching to mainnet.
    let pubkey_for_fetch = identity.pubkey.clone();
    let _ = use_resource(move || {
        let pubkey = pubkey_for_fetch.clone();
        let net = network();
        async move {
            balance.set(None);
            balance_error.set(None);
            tokens.set(Vec::new());
            tokens_error.set(None);
            history.set(Vec::new());
            history_error.set(None);

            let rpc = RpcClient::new(net);
            // Fan out — three independent RPCs.
            let (bal_r, tok_r, hist_r) = futures_util::future::join3(
                rpc.get_balance(&pubkey),
                rpc.get_token_accounts_by_owner(&pubkey),
                rpc.get_signatures_for_address(&pubkey, 20),
            )
            .await;

            match bal_r {
                Ok(lamports) => balance.set(Some(lamports)),
                Err(e) => balance_error.set(Some(e)),
            }
            match tok_r {
                Ok(list) => tokens.set(list),
                Err(e) => tokens_error.set(Some(e)),
            }
            match hist_r {
                Ok(list) => history.set(list),
                Err(e) => history_error.set(Some(e)),
            }
        }
    });

    let pubkey = identity.pubkey.clone();
    let pubkey_copy = pubkey.clone();
    let signing_key = Arc::new(identity.signing_key_clone());
    let from_pubkey_for_send = pubkey.clone();

    rsx! {
        div {
            class: "fixed inset-0 z-50 bg-black/40 fade-in",
            onclick: move |_| on_close.call(()),

            div {
                class: "absolute top-0 right-0 h-full w-[440px] bg-[var(--panel)] border-l border-[var(--border)] flex flex-col text-sm",
                onclick: move |e| e.stop_propagation(),

                // Header
                div { class: "flex items-center justify-between px-4 py-3 border-b border-[var(--border)]",
                    div { class: "flex items-center gap-2",
                        div { class: "text-sm font-medium text-[var(--accent)]", "Wallet" }
                        span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                            "{network().label()}"
                        }
                    }
                    button {
                        class: "text-[var(--text-muted)] hover:text-[var(--text)] text-lg leading-none transition-colors",
                        onclick: move |_| on_close.call(()),
                        "×"
                    }
                }

                // Balance + network selector
                div { class: "px-4 py-3 border-b border-[var(--border)] space-y-2",
                    div { class: "flex items-baseline gap-2",
                        if let Some(lamports) = balance() {
                            span { class: "text-2xl font-medium text-[var(--text)]",
                                "{lamports_to_sol_display(lamports)}"
                            }
                            span { class: "text-xs text-[var(--text-muted)]", "SOL" }
                        } else if balance_error.read().is_some() {
                            span { class: "text-sm text-[var(--danger)]", "balance error" }
                        } else {
                            span { class: "text-sm text-[var(--text-muted)]", "loading…" }
                        }
                    }
                    if let Some(err) = balance_error.read().clone() {
                        div { class: "text-[10px] text-[var(--danger)] break-all", "{err}" }
                    }
                    div { class: "flex gap-1 text-[10px] uppercase tracking-wider",
                        NetworkPill {
                            label: "devnet", active: network() == Network::Devnet,
                            onclick: move |_| network.set(Network::Devnet),
                        }
                        NetworkPill {
                            label: "testnet", active: network() == Network::Testnet,
                            onclick: move |_| network.set(Network::Testnet),
                        }
                        NetworkPill {
                            label: "mainnet", active: network() == Network::MainnetBeta,
                            onclick: move |_| network.set(Network::MainnetBeta),
                        }
                    }
                }

                // Tabs
                div { class: "flex border-b border-[var(--border)] text-xs",
                    TabButton {
                        label: "Receive", active: tab() == Tab::Receive,
                        onclick: move |_| tab.set(Tab::Receive),
                    }
                    TabButton {
                        label: "Send", active: tab() == Tab::Send,
                        onclick: move |_| tab.set(Tab::Send),
                    }
                    TabButton {
                        label: "Activity", active: tab() == Tab::Activity,
                        onclick: move |_| tab.set(Tab::Activity),
                    }
                }

                div { class: "flex-1 overflow-auto p-4",
                    match tab() {
                        Tab::Receive => rsx! {
                            ReceiveTab {
                                pubkey: pubkey.clone(),
                                tokens: tokens(),
                                tokens_error: tokens_error.read().clone(),
                                on_copy: move |_| {
                                    let to_copy = pubkey_copy.clone();
                                    let _ = document::eval(&format!(
                                        "navigator.clipboard.writeText({:?})", to_copy
                                    ));
                                    copied.set(true);
                                },
                                copied: copied(),
                            }
                        },
                        Tab::Send => {
                            let sk = signing_key.clone();
                            let from = from_pubkey_for_send.clone();
                            rsx! {
                                SendTab {
                                    tokens: tokens(),
                                    recipient,
                                    amount,
                                    send_asset,
                                    status: status(),
                                    on_send: move |_| {
                                        do_send(
                                            sk.clone(),
                                            from.clone(),
                                            recipient().trim().to_string(),
                                            amount().trim().to_string(),
                                            send_asset(),
                                            network(),
                                            status,
                                        );
                                    },
                                }
                            }
                        }
                        Tab::Activity => rsx! {
                            ActivityTab {
                                history: history(),
                                error: history_error.read().clone(),
                                network: network(),
                            }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn ReceiveTab(
    pubkey: String,
    tokens: Vec<TokenHolding>,
    tokens_error: Option<String>,
    copied: bool,
    on_copy: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "space-y-4 fade-in",
            div { class: "space-y-2",
                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                    "Your address"
                }
                code { class: "block text-xs text-[var(--text)] bg-[var(--bg)] border border-[var(--border)] rounded p-3 break-all select-all leading-relaxed",
                    "{pubkey}"
                }
                button {
                    class: "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors",
                    onclick: move |_| on_copy.call(()),
                    if copied { "Copied ✓" } else { "Copy address" }
                }
            }

            // Token holdings list. Shown after the address so users see
            // "this address holds X USDC, Y BONK" alongside the receive
            // affordance.
            div { class: "space-y-2",
                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                    "Holdings"
                }
                if let Some(err) = tokens_error.clone() {
                    div { class: "text-[10px] text-[var(--danger)] break-all", "{err}" }
                } else if tokens.is_empty() {
                    p { class: "text-[10px] text-[var(--text-dim)]",
                        "No SPL tokens on this network. Send some to this address from any Solana wallet — they'll show up here."
                    }
                } else {
                    div { class: "space-y-1",
                        for tok in tokens.iter() {
                            TokenRow { tok: tok.clone() }
                        }
                    }
                }
            }

            p { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                "Devnet SOL has no real value — get free airdrops from solfaucet.com to experiment."
            }
        }
    }
}

#[component]
fn TokenRow(tok: TokenHolding) -> Element {
    let mint_short = truncate_middle(&tok.mint, 4, 4);
    rsx! {
        div { class: "flex items-center justify-between gap-2 text-xs border border-[var(--border)] rounded px-2 py-1.5 panel-hover transition-colors",
            div { class: "flex flex-col min-w-0",
                span { class: "text-[var(--text)] font-mono text-[10px] truncate", "{mint_short}" }
                span { class: "text-[var(--text-dim)] text-[9px] uppercase tracking-wider",
                    "{tok.decimals} dec"
                }
            }
            span { class: "text-[var(--text)] tabular-nums font-medium",
                "{tok.ui_amount}"
            }
        }
    }
}

#[component]
fn SendTab(
    tokens: Vec<TokenHolding>,
    recipient: Signal<String>,
    amount: Signal<String>,
    send_asset: Signal<SendAsset>,
    status: Status,
    on_send: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "space-y-3 fade-in",
            // Asset picker — SOL is always there; each token holding gets
            // its own pill.
            div { class: "space-y-1",
                label { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                    "Asset"
                }
                div { class: "flex flex-wrap gap-1",
                    AssetPill {
                        label: "SOL".to_string(),
                        active: matches!(*send_asset.read(), SendAsset::Sol),
                        onclick: move |_| send_asset.set(SendAsset::Sol),
                    }
                    for tok in tokens.iter() {
                        AssetPill {
                            label: truncate_middle(&tok.mint, 4, 4),
                            active: matches!(&*send_asset.read(), SendAsset::Token(t) if t.mint == tok.mint),
                            onclick: {
                                let t = tok.clone();
                                move |_| send_asset.set(SendAsset::Token(t.clone()))
                            },
                        }
                    }
                }
                if let SendAsset::Token(t) = send_asset.read().clone() {
                    p { class: "text-[10px] text-[var(--text-dim)]",
                        "Available: {t.ui_amount}. Recipient must already own this token (have an existing token account) — v1 doesn't auto-create."
                    }
                }
            }

            div { class: "space-y-1",
                label { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                    "Recipient wallet"
                }
                input {
                    class: "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-xs font-mono text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors",
                    r#type: "text",
                    placeholder: "9WzDXwBb…",
                    value: "{recipient}",
                    oninput: move |e| recipient.set(e.value()),
                }
            }
            div { class: "space-y-1",
                label { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                    "Amount"
                }
                input {
                    class: "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors",
                    r#type: "text",
                    placeholder: "0.1",
                    value: "{amount}",
                    oninput: move |e| amount.set(e.value()),
                }
            }

            SendButton { status: status.clone(), onclick: move |_| on_send.call(()) }
            StatusBanner { status: status.clone() }

            p { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                "Transactions sign locally with your identity key. The recipient's address is whatever Solana wallet pubkey they share — your own address from the Receive tab works to test."
            }
        }
    }
}

#[component]
fn ActivityTab(history: Vec<TxRecord>, error: Option<String>, network: Network) -> Element {
    rsx! {
        div { class: "space-y-2 fade-in",
            if let Some(err) = error {
                div { class: "text-[10px] text-[var(--danger)] break-all", "{err}" }
            } else if history.is_empty() {
                p { class: "text-xs text-[var(--text-dim)]",
                    "No transactions yet on this network."
                }
            } else {
                for tx in history.iter() {
                    ActivityRow { tx: tx.clone(), network }
                }
            }
        }
    }
}

#[component]
fn ActivityRow(tx: TxRecord, network: Network) -> Element {
    let sig_short = truncate_middle(&tx.signature, 6, 6);
    let when = tx
        .block_time
        .map(|t| format_unix_relative(t))
        .unwrap_or_else(|| "pending".to_string());
    let (label, color) = if tx.err {
        ("failed", "text-[var(--danger)]")
    } else {
        ("ok", "text-[var(--success)]")
    };
    let url = network.explorer_tx_url(&tx.signature);
    rsx! {
        a {
            href: "{url}",
            target: "_blank",
            class: "block border border-[var(--border)] hover:border-[var(--accent)] rounded px-2 py-1.5 text-xs panel-hover transition-colors",
            div { class: "flex items-center justify-between gap-2",
                code { class: "font-mono text-[var(--text)] truncate", "{sig_short}" }
                span { class: "text-[10px] uppercase tracking-wider {color}", "{label}" }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mt-0.5", "{when}" }
        }
    }
}

/// Dispatch a send: parses the amount, decides SOL vs token, kicks off
/// the async RPC call, mutates `status`. Pulled out of the SendTab event
/// handler to keep the rsx! readable.
fn do_send(
    signing_key: Arc<ed25519_dalek::SigningKey>,
    from_wallet: String,
    recipient: String,
    amount_str: String,
    asset: SendAsset,
    network: Network,
    mut status: Signal<Status>,
) {
    let parsed = match amount_str.parse::<f64>() {
        Ok(n) => n,
        Err(_) => {
            status.set(Status::Failed("amount must be a number".into()));
            return;
        }
    };

    status.set(Status::Sending);
    spawn(async move {
        let rpc = RpcClient::new(network);
        let result = match asset {
            SendAsset::Sol => {
                let Some(lamports) = sol_to_lamports(parsed) else {
                    status.set(Status::Failed("amount must be positive".into()));
                    return;
                };
                send_sol(&rpc, &signing_key, &from_wallet, &recipient, lamports).await
            }
            SendAsset::Token(tok) => {
                let Some(raw) = ui_amount_to_raw(parsed, tok.decimals) else {
                    status.set(Status::Failed("amount must be positive".into()));
                    return;
                };
                send_spl_token(&rpc, &signing_key, &from_wallet, &recipient, &tok.mint, raw).await
            }
        };
        match result {
            Ok(sig) => {
                let url = network.explorer_tx_url(&sig);
                status.set(Status::Sent {
                    signature: sig,
                    explorer_url: url,
                });
            }
            Err(e) => status.set(Status::Failed(e)),
        }
    });
}

// -------- small UI primitives ---------------------------------------------

#[component]
fn NetworkPill(label: &'static str, active: bool, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)]"
    };
    rsx! {
        button {
            class: "border rounded px-2 py-1 transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn AssetPill(label: String, active: bool, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)]"
    };
    rsx! {
        button {
            class: "border rounded px-2 py-1 text-[10px] uppercase tracking-wider font-mono transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn TabButton(label: &'static str, active: bool, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "border-b-2 border-[var(--accent)] text-[var(--text)]"
    } else {
        "border-b-2 border-transparent text-[var(--text-muted)] hover:text-[var(--text)]"
    };
    rsx! {
        button {
            class: "flex-1 px-4 py-2 text-xs uppercase tracking-wider transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn SendButton(status: Status, onclick: EventHandler<()>) -> Element {
    let busy = matches!(status, Status::Sending);
    rsx! {
        button {
            class: "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
            disabled: busy,
            onclick: move |_| onclick.call(()),
            if busy { "Sending…" } else { "Send" }
        }
    }
}

#[component]
fn StatusBanner(status: Status) -> Element {
    match status {
        Status::Idle | Status::Sending => rsx! { Fragment {} },
        Status::Sent { signature, explorer_url } => rsx! {
            div { class: "border border-[var(--success)] rounded p-2 text-xs space-y-1",
                div { class: "text-[var(--success)]", "Transaction submitted" }
                code { class: "block text-[10px] text-[var(--text-muted)] break-all font-mono",
                    "{signature}"
                }
                a {
                    href: "{explorer_url}",
                    target: "_blank",
                    class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] underline",
                    "View on Solana Explorer →"
                }
            }
        },
        Status::Failed(err) => rsx! {
            div { class: "border border-[var(--danger)] rounded p-2 text-xs text-[var(--danger)] break-all",
                "{err}"
            }
        },
    }
}

// -------- helpers ---------------------------------------------------------

fn truncate_middle(s: &str, head: usize, tail: usize) -> String {
    if s.chars().count() <= head + tail + 1 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let head_str: String = chars.iter().take(head).collect();
    let tail_str: String = chars.iter().rev().take(tail).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head_str}…{tail_str}")
}

/// Best-effort relative formatting of a unix timestamp ("5m ago"). Falls
/// back to the raw timestamp if the chrono diff is weird (clock skew, etc.).
fn format_unix_relative(secs: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - secs;
    if diff < 0 {
        return "future".to_string();
    }
    let s = diff;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}
