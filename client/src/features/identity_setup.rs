//! First-launch identity setup. Creates a new Ed25519 keypair (with BIP39
//! recovery phrase) or restores from an existing one.

use dioxus::prelude::*;

use crate::identity::Identity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Choose,
    Create { generated: bool },
    Restore,
}

const PANEL: &str = "bg-[var(--panel)] border border-[var(--border)] rounded-lg";
const INPUT: &str = "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn IdentitySetupView(on_done: EventHandler<Identity>) -> Element {
    let mut step = use_signal(|| Step::Choose);
    let mut display_name = use_signal(|| String::new());
    let mut draft_identity = use_signal(|| None::<Identity>);
    let mut error = use_signal(|| None::<String>);
    let mut reveal = use_signal(|| false);
    let mut restore_phrase = use_signal(|| String::new());

    let step_key = format!("step-{:?}", step());

    rsx! {
        div { class: "h-full w-full flex items-center justify-center bg-[var(--bg)] p-4",
            div { class: "w-full max-w-md {PANEL} p-6 space-y-5 min-h-[520px] flex flex-col",

                div { class: "space-y-1",
                    h1 { class: "text-lg font-semibold text-[var(--accent)]", "Set up your identity" }
                    p { class: "text-xs text-[var(--text-muted)]",
                        "Your identity is universal — same address across every server you join. Pick one option below."
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
                            button {
                                r#type: "button",
                                class: "w-full text-left border border-[var(--border)] hover:border-[var(--accent)] rounded p-3 transition-colors group",
                                onclick: move |_| {
                                    error.set(None);
                                    match Identity::create(default_name()) {
                                        Ok(id) => {
                                            display_name.set(id.display_name.clone());
                                            draft_identity.set(Some(id));
                                            step.set(Step::Create { generated: true });
                                        }
                                        Err(e) => error.set(Some(e)),
                                    }
                                },
                                div { class: "text-sm text-[var(--text)] group-hover:text-[var(--accent)]",
                                    "Create new identity"
                                }
                                div { class: "text-xs text-[var(--text-muted)] mt-1",
                                    "Generate a fresh Solana-format keypair. You'll get a 12-word recovery phrase to save."
                                }
                            }
                            button {
                                r#type: "button",
                                class: "w-full text-left border border-[var(--border)] hover:border-[var(--accent)] rounded p-3 transition-colors group",
                                onclick: move |_| {
                                    error.set(None);
                                    step.set(Step::Restore);
                                },
                                div { class: "text-sm text-[var(--text)] group-hover:text-[var(--accent)]",
                                    "Restore from seed phrase"
                                }
                                div { class: "text-xs text-[var(--text-muted)] mt-1",
                                    "Paste an existing 12-word BIP39 phrase (works with Phantom, Solflare, etc.)."
                                }
                            }
                        }
                    },

                    Step::Create { .. } => {
                        let identity = draft_identity.read().clone();
                        let identity = identity.unwrap();
                        let pubkey = identity.pubkey.clone();
                        let phrase = identity.seed_phrase.clone();
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
                                    class: "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors disabled:opacity-30 disabled:cursor-not-allowed",
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
                                button {
                                    r#type: "button",
                                    class: "w-full text-xs text-[var(--text-muted)] hover:text-[var(--text)] py-1",
                                    onclick: move |_| {
                                        draft_identity.set(None);
                                        step.set(Step::Choose);
                                    },
                                    "← back"
                                }
                            }
                        }
                    },

                    Step::Restore => rsx! {
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
                                class: "w-full bg-[var(--accent)] hover:bg-[var(--accent-strong)] text-[#0a0908] font-medium py-2 rounded text-sm transition-colors disabled:opacity-30 disabled:cursor-not-allowed",
                                disabled: display_name().trim().is_empty() || restore_phrase().trim().is_empty(),
                                onclick: move |_| {
                                    let name = display_name().trim().to_string();
                                    let phrase = restore_phrase().trim().to_string();
                                    match Identity::restore(&phrase, &name) {
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
                            button {
                                r#type: "button",
                                class: "w-full text-xs text-[var(--text-muted)] hover:text-[var(--text)] py-1",
                                onclick: move |_| {
                                    restore_phrase.set(String::new());
                                    step.set(Step::Choose);
                                },
                                "← back"
                            }
                        }
                    },
                }
                }
            }
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
