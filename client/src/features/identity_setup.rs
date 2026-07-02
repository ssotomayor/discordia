//! First-launch identity setup. Three paths: create a new BIP39-derived
//! identity, restore one from a 12-word phrase, or import a raw private key
//! (32 or 64 base58 bytes — Phantom/Solflare export format).

use dioxus::prelude::*;

use crate::identity::{Identity, IdentitySource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Choose,
    Create,
    RestorePhrase,
    ImportKey,
}

const INPUT: &str = "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn IdentitySetupView(on_done: EventHandler<Identity>) -> Element {
    let mut step = use_signal(|| Step::Choose);
    let mut display_name = use_signal(String::new);
    let mut draft_identity = use_signal(|| None::<Identity>);
    let mut error = use_signal(|| None::<String>);
    let mut reveal = use_signal(|| false);
    let mut restore_phrase = use_signal(String::new);
    let mut private_key_input = use_signal(String::new);

    let step_key = format!("step-{:?}", step());
    let mac_top_pad = if cfg!(target_os = "macos") { "pt-7" } else { "pt-0" };

    rsx! {
        div { class: "h-full w-full flex bg-[var(--bg)]",
            // Brand panel matches the Connect screen so first-launch
            // feels like the same app.
            div {
                class: "dxf-drag-region hidden md:flex w-1/3 min-w-[300px] max-w-[440px] flex-col items-center justify-center px-8 border-r border-[var(--border)]",
                onmousedown: move |_| crate::app::start_window_drag(),
                crate::app::DiscordiaLogo { class: "w-32 h-32 mb-4" }
                h1 { class: "text-2xl font-semibold text-[var(--accent)] tracking-tight",
                    "Discordia"
                }
                p { class: "text-xs text-[var(--text-muted)] mt-3 text-center max-w-[260px] leading-relaxed",
                    "Welcome. Pick or create an identity to get started."
                }
            }

            div { class: "flex-1 flex flex-col overflow-hidden min-w-0",
                div {
                    class: "dxf-drag-region h-8 shrink-0 {mac_top_pad}",
                    onmousedown: move |_| crate::app::start_window_drag(),
                }
                div { class: "flex-1 overflow-auto px-8 pb-8 dxf-no-drag",
                    div { class: "w-full max-w-md mx-auto space-y-5",

                div { class: "space-y-1",
                    h1 { class: "text-lg font-semibold text-[var(--accent)]", "Set up your identity" }
                    p { class: "text-xs text-[var(--text-muted)]",
                        "Your identity is universal — same address across every server you join."
                    }
                }

                div { class: "h-px bg-[var(--border)]" }

                if let Some(err) = error.read().clone() {
                    div { class: "text-xs text-[var(--danger)] border border-[var(--border)] rounded px-3 py-2",
                        "{err}"
                    }
                }

                div { key: "{step_key}", class: "fade-in flex-1",
                match step() {
                    Step::Choose => rsx! {
                        div { class: "space-y-2",
                            ChooseOption {
                                title: "Create new identity",
                                blurb: "Generate a fresh Nostr keypair. You'll get a 12-word recovery phrase to save.",
                                onclick: move |_| {
                                    error.set(None);
                                    match Identity::create(default_name()) {
                                        Ok(id) => {
                                            display_name.set(id.display_name.clone());
                                            draft_identity.set(Some(id));
                                            step.set(Step::Create);
                                        }
                                        Err(e) => error.set(Some(e)),
                                    }
                                },
                            }
                            ChooseOption {
                                title: "Restore from seed phrase",
                                blurb: "Paste a 12-word BIP39 phrase. Works with any NIP-06 Nostr wallet.",
                                onclick: move |_| {
                                    error.set(None);
                                    step.set(Step::RestorePhrase);
                                },
                            }
                            ChooseOption {
                                title: "Import private key",
                                blurb: "Paste an nsec (bech32) or a 64-char hex secret key.",
                                onclick: move |_| {
                                    error.set(None);
                                    step.set(Step::ImportKey);
                                },
                            }
                        }
                    },

                    Step::Create => {
                        let identity = draft_identity.read().clone().unwrap();
                        let pubkey = identity.pubkey.clone();
                        let phrase = match &identity.source {
                            IdentitySource::Phrase(p) => p.clone(),
                            _ => String::new(),
                        };
                        let phrase_display = if reveal() { phrase.clone() } else { dot_mask(&phrase) };
                        rsx! {
                            div { class: "space-y-3",
                                div { class: "space-y-1",
                                    div { class: LABEL, "Your pubkey" }
                                    code { class: "block text-xs text-[var(--text)] bg-[var(--bg)] border border-[var(--border)] rounded p-2 break-all select-all",
                                        "{pubkey}"
                                    }
                                }

                                div { class: "space-y-1",
                                    div { class: "flex items-center gap-2",
                                        span { class: LABEL, "Recovery phrase" }
                                        span { class: "flex-1" }
                                        button {
                                            r#type: "button",
                                            class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] uppercase tracking-wider",
                                            onclick: move |_| reveal.set(!reveal()),
                                            if reveal() { "Hide" } else { "Reveal" }
                                        }
                                    }
                                    div { class: "text-xs text-[var(--text)] bg-[var(--bg)] border border-[var(--border)] rounded p-3 leading-relaxed select-all break-words",
                                        "{phrase_display}"
                                    }
                                    p { class: "text-[10px] text-[var(--warn)]",
                                        "Write this down. It's the only way to restore your identity if you lose this device."
                                    }
                                }

                                div { class: "space-y-1",
                                    label { class: LABEL, "Display name" }
                                    input {
                                        class: INPUT,
                                        r#type: "text",
                                        value: "{display_name}",
                                        oninput: move |e| display_name.set(e.value()),
                                    }
                                }

                                button {
                                    r#type: "button",
                                    class: PRIMARY_BUTTON,
                                    disabled: display_name().trim().is_empty(),
                                    onclick: move |_| {
                                        let name = display_name().trim().to_string();
                                        if name.is_empty() { return; }
                                        let Some(mut id) = draft_identity().clone() else { return };
                                        id.display_name = name;
                                        if let Err(e) = id.save() {
                                            error.set(Some(format!("save failed: {e}")));
                                            return;
                                        }
                                        on_done.call(id);
                                    },
                                    "I've saved my phrase — continue"
                                }
                                BackButton { onclick: move |_| {
                                    draft_identity.set(None);
                                    step.set(Step::Choose);
                                } }
                            }
                        }
                    },

                    Step::RestorePhrase => rsx! {
                        div { class: "space-y-3",
                            div { class: "space-y-1",
                                label { class: LABEL, "12-word recovery phrase" }
                                textarea {
                                    class: "{INPUT} resize-none h-24 lowercase",
                                    placeholder: "apple banana cherry dog ...",
                                    value: "{restore_phrase}",
                                    oninput: move |e| restore_phrase.set(e.value()),
                                }
                            }
                            div { class: "space-y-1",
                                label { class: LABEL, "Display name" }
                                input {
                                    class: INPUT,
                                    r#type: "text",
                                    placeholder: "your-handle",
                                    value: "{display_name}",
                                    oninput: move |e| display_name.set(e.value()),
                                }
                            }
                            button {
                                r#type: "button",
                                class: PRIMARY_BUTTON,
                                disabled: display_name().trim().is_empty() || restore_phrase().trim().is_empty(),
                                onclick: move |_| {
                                    let name = display_name().trim().to_string();
                                    let phrase = restore_phrase().trim().to_string();
                                    match Identity::restore_from_phrase(&phrase, &name) {
                                        Ok(id) => {
                                            if let Err(e) = id.save() {
                                                error.set(Some(format!("save failed: {e}")));
                                                return;
                                            }
                                            on_done.call(id);
                                        }
                                        Err(e) => error.set(Some(e)),
                                    }
                                },
                                "Restore identity"
                            }
                            BackButton { onclick: move |_| {
                                restore_phrase.set(String::new());
                                step.set(Step::Choose);
                            } }
                        }
                    },

                    Step::ImportKey => rsx! {
                        div { class: "space-y-3",
                            div { class: "space-y-1",
                                label { class: LABEL, "Private key (nsec or hex)" }
                                textarea {
                                    class: "{INPUT} resize-none h-20 font-mono text-[11px]",
                                    placeholder: "nsec1… or 64-char hex",
                                    value: "{private_key_input}",
                                    oninput: move |e| private_key_input.set(e.value()),
                                }
                                p { class: "text-[10px] text-[var(--text-dim)]",
                                    "Accepts an nsec (bech32) or a raw 32-byte secret as 64 hex chars."
                                }
                            }
                            div { class: "space-y-1",
                                label { class: LABEL, "Display name" }
                                input {
                                    class: INPUT,
                                    r#type: "text",
                                    placeholder: "your-handle",
                                    value: "{display_name}",
                                    oninput: move |e| display_name.set(e.value()),
                                }
                            }
                            div { class: "text-[10px] text-[var(--warn)] border border-[var(--border)] rounded p-2",
                                "Keys imported this way have no seed phrase. Back up the key string itself if you want to restore later."
                            }
                            button {
                                r#type: "button",
                                class: PRIMARY_BUTTON,
                                disabled: display_name().trim().is_empty() || private_key_input().trim().is_empty(),
                                onclick: move |_| {
                                    let name = display_name().trim().to_string();
                                    let key = private_key_input().trim().to_string();
                                    match Identity::restore_from_private_key(&key, &name) {
                                        Ok(id) => {
                                            if let Err(e) = id.save() {
                                                error.set(Some(format!("save failed: {e}")));
                                                return;
                                            }
                                            on_done.call(id);
                                        }
                                        Err(e) => error.set(Some(e)),
                                    }
                                },
                                "Import identity"
                            }
                            BackButton { onclick: move |_| {
                                private_key_input.set(String::new());
                                step.set(Step::Choose);
                            } }
                        }
                    },
                }
                }
                    }
                }
            }
        }
    }
}

const PRIMARY_BUTTON: &str = "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors disabled:opacity-30 disabled:cursor-not-allowed";

#[component]
fn ChooseOption(title: &'static str, blurb: &'static str, onclick: EventHandler<()>) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "panel-hover w-full text-left border border-[var(--border)] hover:border-[var(--accent)] rounded p-3 group",
            onclick: move |_| onclick.call(()),
            div { class: "text-sm text-[var(--text)] group-hover:text-[var(--accent)] transition-colors",
                "{title}"
            }
            div { class: "text-xs text-[var(--text-muted)] mt-1", "{blurb}" }
        }
    }
}

#[component]
fn BackButton(onclick: EventHandler<()>) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "w-full text-xs text-[var(--text-muted)] hover:text-[var(--text)] py-1 transition-colors",
            onclick: move |_| onclick.call(()),
            "← back"
        }
    }
}

fn default_name() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let suffix = format!("{:02x}{:02x}", bytes[0], bytes[1]);
    format!("user-{suffix}")
}

fn dot_mask(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { c } else { '•' })
        .collect()
}
