//! In-app Solana wallet — a small icon top-right of the workspace that
//! opens a drawer for receive (show address) and send (sign + submit a
//! `System.Transfer`). Network selector lets users switch between devnet
//! (default, safe to experiment), testnet, and mainnet-beta.

use std::sync::Arc;

use dioxus::prelude::*;

use crate::identity::Identity;
use crate::wallet::{Network, RpcClient, lamports_to_sol_display, send_sol, sol_to_lamports};

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
            // Inline SVG wallet icon — keeps zero asset deps.
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Sending,
    Sent { signature: String, explorer_url: String },
    Failed(String),
}

#[component]
fn WalletDrawer(identity: Identity, on_close: EventHandler<()>) -> Element {
    let mut tab = use_signal(|| Tab::Receive);
    let mut network = use_signal(|| Network::Devnet);
    let mut balance = use_signal(|| None::<u64>);
    let mut balance_error = use_signal(|| None::<String>);
    let mut copied = use_signal(|| false);
    let mut recipient = use_signal(String::new);
    let mut amount = use_signal(String::new);
    let mut status = use_signal(|| Status::Idle);

    let pubkey_for_balance = identity.pubkey.clone();

    // Refetch balance whenever the network changes or the drawer opens.
    let _ = use_resource(move || {
        let pubkey = pubkey_for_balance.clone();
        let net = network();
        async move {
            balance.set(None);
            balance_error.set(None);
            let rpc = RpcClient::new(net);
            match rpc.get_balance(&pubkey).await {
                Ok(lamports) => balance.set(Some(lamports)),
                Err(e) => balance_error.set(Some(e)),
            }
        }
    });

    let pubkey = identity.pubkey.clone();
    let pubkey_copy = pubkey.clone();
    let signing_key = Arc::new(identity.signing_key_clone());
    let from_pubkey_for_send = pubkey.clone();

    rsx! {
        div {
            // Backdrop — click anywhere outside the drawer to close.
            class: "fixed inset-0 z-50 bg-black/40 fade-in",
            onclick: move |_| on_close.call(()),

            div {
                // Drawer pinned to the right side of the screen. stopPropagation
                // so clicks inside don't bubble to the backdrop.
                class: "absolute top-0 right-0 h-full w-[420px] bg-[var(--panel)] border-l border-[var(--border)] flex flex-col text-sm",
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
                }

                // Tab content
                div { class: "flex-1 overflow-auto p-4",
                    match tab() {
                        Tab::Receive => rsx! {
                            div { class: "space-y-3 fade-in",
                                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                                    "Your address"
                                }
                                code { class: "block text-xs text-[var(--text)] bg-[var(--bg)] border border-[var(--border)] rounded p-3 break-all select-all leading-relaxed",
                                    "{pubkey}"
                                }
                                button {
                                    class: "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors",
                                    onclick: move |_| {
                                        let to_copy = pubkey_copy.clone();
                                        let _ = document::eval(&format!(
                                            "navigator.clipboard.writeText({:?})",
                                            to_copy
                                        ));
                                        copied.set(true);
                                    },
                                    if copied() { "Copied ✓" } else { "Copy address" }
                                }
                                p { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                                    "Send SOL or SPL tokens to this address from any Solana wallet. Devnet SOL has no real value — get free airdrops from solfaucet.com to experiment."
                                }
                            }
                        },
                        Tab::Send => {
                            let signing_key_for_click = signing_key.clone();
                            let from_pubkey_for_click = from_pubkey_for_send.clone();
                            rsx! {
                                div { class: "space-y-3 fade-in",
                                    div { class: "space-y-1",
                                        label { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                                            "Recipient address"
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
                                            "Amount (SOL)"
                                        }
                                        input {
                                            class: "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors",
                                            r#type: "text",
                                            placeholder: "0.1",
                                            value: "{amount}",
                                            oninput: move |e| amount.set(e.value()),
                                        }
                                    }

                                    SendButton {
                                        status: status(),
                                        onclick: move |_| {
                                            let sk = signing_key_for_click.clone();
                                            let from = from_pubkey_for_click.clone();
                                            let to = recipient().trim().to_string();
                                            let amount_str = amount().trim().to_string();
                                            let net = network();

                                            // Parse amount client-side so the error
                                            // surfaces immediately (no RPC roundtrip
                                            // wasted on a bad number).
                                            let lamports = match amount_str.parse::<f64>() {
                                                Ok(sol) => match sol_to_lamports(sol) {
                                                    Some(l) => l,
                                                    None => {
                                                        status.set(Status::Failed(
                                                            "amount must be positive".into(),
                                                        ));
                                                        return;
                                                    }
                                                },
                                                Err(_) => {
                                                    status.set(Status::Failed(
                                                        "amount must be a number".into(),
                                                    ));
                                                    return;
                                                }
                                            };

                                            status.set(Status::Sending);
                                            spawn(async move {
                                                let rpc = RpcClient::new(net);
                                                match send_sol(&rpc, &sk, &from, &to, lamports).await {
                                                    Ok(sig) => {
                                                        let url = net.explorer_tx_url(&sig);
                                                        status.set(Status::Sent {
                                                            signature: sig,
                                                            explorer_url: url,
                                                        });
                                                    }
                                                    Err(e) => status.set(Status::Failed(e)),
                                                }
                                            });
                                        },
                                    }

                                    StatusBanner { status: status() }

                                    p { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                                        "Transactions sign locally with your identity key. The recipient's address is whatever Solana pubkey they share with you — your own address from the Receive tab works to test."
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

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
