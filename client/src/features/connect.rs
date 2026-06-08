use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::{SessionMode, SessionParams};

// 7700 because macOS reserves 7000 for AirPlay Receiver.
const DEFAULT_RENDEZVOUS_URL: &str = "ws://localhost:7700";

/// Mirror of `dioxusfun_rendezvous::protocol::DiscoverEntry`. Re-declared
/// locally to keep the client crate free of the rendezvous-server dep.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DiscoverEntry {
    shortcode: String,
    name: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Browse,
    ByCode,
    SelfHost,
    Remote,
}

#[component]
pub fn ConnectView(error: Option<String>, on_connect: EventHandler<SessionParams>) -> Element {
    let default_rendezvous = std::env::var("DIOXUSFUN_RENDEZVOUS_URL")
        .ok()
        .unwrap_or_else(|| DEFAULT_RENDEZVOUS_URL.to_string());

    let mut tab = use_signal(|| Tab::Browse);
    let mut server_url = use_signal(|| "ws://localhost:9000".to_string());
    let mut username = use_signal(String::new);
    let mut allow_lan = use_signal(|| false);
    let mut publish_to_rendezvous = use_signal(|| true);
    let mut rendezvous_url = use_signal(|| default_rendezvous.clone());
    let mut code = use_signal(String::new);

    // Self-host public listing fields
    let mut publish_name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut publish_public = use_signal(|| false);

    let submit = move |_| {
        let name = username().trim().to_string();
        if name.is_empty() {
            return;
        }
        let params = match tab() {
            Tab::Remote => {
                let url = server_url().trim().to_string();
                if url.is_empty() {
                    return;
                }
                SessionParams {
                    mode: SessionMode::Remote { server_url: url },
                    username: name,
                }
            }
            Tab::SelfHost => {
                let r_url = if publish_to_rendezvous() {
                    let r = rendezvous_url().trim().to_string();
                    if r.is_empty() { None } else { Some(r) }
                } else {
                    None
                };
                let pn = publish_name().trim().to_string();
                let desc = description().trim().to_string();
                SessionParams {
                    mode: SessionMode::SelfHost {
                        allow_lan: allow_lan(),
                        rendezvous_url: r_url,
                        publish_name: if pn.is_empty() { None } else { Some(pn) },
                        description: if desc.is_empty() { None } else { Some(desc) },
                        publish_public: publish_to_rendezvous() && publish_public(),
                    },
                    username: name,
                }
            }
            Tab::ByCode | Tab::Browse => {
                let c = code().trim().to_string();
                let r = rendezvous_url().trim().to_string();
                if c.is_empty() || r.is_empty() {
                    return;
                }
                SessionParams {
                    mode: SessionMode::ByCode {
                        rendezvous_url: r,
                        code: c,
                    },
                    username: name,
                }
            }
        };
        on_connect.call(params);
    };

    let disabled = match tab() {
        Tab::Remote => server_url().trim().is_empty() || username().trim().is_empty(),
        Tab::SelfHost => username().trim().is_empty(),
        Tab::ByCode | Tab::Browse => {
            code().trim().is_empty()
                || rendezvous_url().trim().is_empty()
                || username().trim().is_empty()
        }
    };

    rsx! {
        div { class: "h-full w-full flex items-center justify-center bg-gradient-to-br from-[#1e1f22] via-[#2b2d31] to-[#1e1f22]",
            form {
                class: "w-full max-w-md bg-[#2b2d31] border border-white/5 rounded-2xl shadow-2xl p-8 space-y-5",
                onsubmit: submit,

                div { class: "text-center space-y-1",
                    h1 { class: "text-2xl font-bold text-white", "Welcome to dioxusfun" }
                    p { class: "text-sm text-gray-400",
                        "Browse public servers, join by code, host your own, or paste a URL."
                    }
                }

                div { class: "flex gap-1 p-1 bg-[#1e1f22] rounded-lg text-[13px]",
                    TabButton { active: tab() == Tab::Browse, label: "Browse", onclick: move |_| tab.set(Tab::Browse) }
                    TabButton { active: tab() == Tab::ByCode, label: "By code", onclick: move |_| tab.set(Tab::ByCode) }
                    TabButton { active: tab() == Tab::SelfHost, label: "Self-host", onclick: move |_| tab.set(Tab::SelfHost) }
                    TabButton { active: tab() == Tab::Remote, label: "URL", onclick: move |_| tab.set(Tab::Remote) }
                }

                if let Some(err) = error {
                    div { class: "text-sm text-red-300 bg-red-900/30 border border-red-700/40 rounded-lg px-3 py-2",
                        "{err}"
                    }
                }

                match tab() {
                    Tab::Browse => rsx! {
                        BrowseTab {
                            rendezvous_url: rendezvous_url(),
                            on_rendezvous_change: move |s: String| rendezvous_url.set(s),
                            on_pick: move |entry: DiscoverEntry| {
                                code.set(entry.shortcode);
                            },
                            picked_shortcode: code(),
                        }
                    },
                    Tab::ByCode => rsx! {
                        div { class: "space-y-2",
                            div { class: "space-y-1",
                                label { class: "text-xs font-semibold uppercase tracking-wide text-gray-400",
                                    "Shortcode"
                                }
                                input {
                                    class: "w-full bg-[#1e1f22] border border-white/5 rounded-md px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-400 lowercase",
                                    r#type: "text",
                                    placeholder: "purple-fox-42",
                                    value: "{code}",
                                    oninput: move |e| code.set(e.value()),
                                }
                            }
                            details { class: "text-xs text-gray-500",
                                summary { class: "cursor-pointer hover:text-gray-300", "Rendezvous server (advanced)" }
                                input {
                                    class: "mt-2 w-full bg-[#1e1f22] border border-white/5 rounded-md px-3 py-1.5 text-xs text-white focus:outline-none focus:border-indigo-400",
                                    r#type: "text",
                                    placeholder: "ws://localhost:7700",
                                    value: "{rendezvous_url}",
                                    oninput: move |e| rendezvous_url.set(e.value()),
                                }
                            }
                        }
                    },
                    Tab::SelfHost => rsx! {
                        div { class: "rounded-md border border-emerald-700/40 bg-emerald-900/20 p-3 text-xs text-emerald-200 space-y-2",
                            div { "Your machine runs the gateway, voice SFU, and (optionally) publishes a shortcode through a rendezvous so friends can join without your IP." }
                            label { class: "flex items-center gap-2 cursor-pointer text-emerald-100/90",
                                input {
                                    r#type: "checkbox",
                                    checked: publish_to_rendezvous(),
                                    oninput: move |e| publish_to_rendezvous.set(e.value() == "true"),
                                }
                                "Publish a shortcode via rendezvous"
                            }
                            label { class: "flex items-center gap-2 cursor-pointer text-emerald-100/90",
                                input {
                                    r#type: "checkbox",
                                    checked: allow_lan(),
                                    oninput: move |e| allow_lan.set(e.value() == "true"),
                                }
                                "Also let LAN friends connect directly"
                            }
                            if publish_to_rendezvous() {
                                div { class: "pl-4 border-l-2 border-emerald-700/40 space-y-2 mt-2",
                                    div { class: "space-y-1",
                                        label { class: "text-[11px] uppercase tracking-wide text-emerald-300/80", "Server name" }
                                        input {
                                            class: "w-full bg-[#1e1f22] border border-white/5 rounded px-2 py-1 text-emerald-100 focus:outline-none focus:border-indigo-400",
                                            r#type: "text",
                                            placeholder: "My Awesome Server",
                                            value: "{publish_name}",
                                            oninput: move |e| publish_name.set(e.value()),
                                        }
                                    }
                                    div { class: "space-y-1",
                                        label { class: "text-[11px] uppercase tracking-wide text-emerald-300/80", "Description (optional)" }
                                        input {
                                            class: "w-full bg-[#1e1f22] border border-white/5 rounded px-2 py-1 text-emerald-100 focus:outline-none focus:border-indigo-400",
                                            r#type: "text",
                                            placeholder: "Friends-only Rust chat",
                                            value: "{description}",
                                            oninput: move |e| description.set(e.value()),
                                        }
                                    }
                                    label { class: "flex items-center gap-2 cursor-pointer text-emerald-100/90",
                                        input {
                                            r#type: "checkbox",
                                            checked: publish_public(),
                                            oninput: move |e| publish_public.set(e.value() == "true"),
                                        }
                                        "List this server in the public Browse tab"
                                    }
                                    details { class: "text-[11px] text-emerald-300/80",
                                        summary { class: "cursor-pointer", "Rendezvous URL" }
                                        input {
                                            class: "mt-1 w-full bg-[#1e1f22] border border-emerald-700/40 rounded px-2 py-1 text-emerald-100 focus:outline-none",
                                            r#type: "text",
                                            placeholder: "ws://localhost:7700",
                                            value: "{rendezvous_url}",
                                            oninput: move |e| rendezvous_url.set(e.value()),
                                        }
                                    }
                                }
                            }
                        }
                    },
                    Tab::Remote => rsx! {
                        div { class: "space-y-1",
                            label { class: "text-xs font-semibold uppercase tracking-wide text-gray-400",
                                "Server URL"
                            }
                            input {
                                class: "w-full bg-[#1e1f22] border border-white/5 rounded-md px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-400",
                                r#type: "text",
                                placeholder: "ws://localhost:9000",
                                value: "{server_url}",
                                oninput: move |e| server_url.set(e.value()),
                            }
                        }
                    },
                }

                div { class: "space-y-1",
                    label { class: "text-xs font-semibold uppercase tracking-wide text-gray-400",
                        "Display name"
                    }
                    input {
                        class: "w-full bg-[#1e1f22] border border-white/5 rounded-md px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-400",
                        r#type: "text",
                        placeholder: "your-handle",
                        value: "{username}",
                        oninput: move |e| username.set(e.value()),
                    }
                }

                button {
                    class: "w-full bg-indigo-500 hover:bg-indigo-400 text-white font-semibold py-2 rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
                    r#type: "submit",
                    disabled,
                    {match tab() {
                        Tab::Browse => "Join selected",
                        Tab::ByCode => "Join",
                        Tab::Remote => "Connect",
                        Tab::SelfHost => "Launch",
                    }}
                }
            }
        }
    }
}

#[component]
fn TabButton(active: bool, label: &'static str, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "bg-indigo-500 text-white"
    } else {
        "text-gray-400 hover:text-gray-200"
    };
    rsx! {
        button {
            r#type: "button",
            class: "flex-1 px-3 py-1.5 rounded-md font-semibold transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn BrowseTab(
    rendezvous_url: String,
    on_rendezvous_change: EventHandler<String>,
    on_pick: EventHandler<DiscoverEntry>,
    picked_shortcode: String,
) -> Element {
    // Re-fetch whenever the rendezvous URL changes or we manually bump the
    // refresh counter.
    let mut refresh_tick = use_signal(|| 0u32);
    let url_for_fetch = rendezvous_url.clone();
    let entries = use_resource(move || {
        let _ = refresh_tick();
        let base = ws_to_http(&url_for_fetch);
        async move {
            let url = format!("{base}/discover");
            reqwest::Client::new()
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("fetch: {e}"))?
                .json::<Vec<DiscoverEntry>>()
                .await
                .map_err(|e| format!("decode: {e}"))
        }
    });

    rsx! {
        div { class: "space-y-3",
            div { class: "flex items-center gap-2",
                span { class: "text-xs font-semibold uppercase tracking-wide text-gray-400 flex-1",
                    "Public servers"
                }
                button {
                    r#type: "button",
                    class: "text-xs text-indigo-300 hover:text-indigo-200",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "↻ Refresh"
                }
            }

            div { class: "max-h-72 overflow-y-auto space-y-1 bg-[#1e1f22] border border-white/5 rounded-md p-1",
                match &*entries.read_unchecked() {
                    None => rsx! {
                        div { class: "text-xs text-gray-500 px-3 py-4 text-center", "Loading…" }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "text-xs text-red-300 px-3 py-4",
                            "Couldn't reach rendezvous: {e}"
                        }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div { class: "text-xs text-gray-500 px-3 py-4 text-center",
                            "No public servers yet. Pick Self-host and check \"List publicly\" to put one here."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for entry in list.iter().cloned() {
                            {
                                let sc = entry.shortcode.clone();
                                let selected = picked_shortcode == sc;
                                let row_cls = if selected {
                                    "bg-indigo-500/20 border-indigo-400/50"
                                } else {
                                    "bg-transparent border-transparent hover:bg-white/5"
                                };
                                let entry_for_pick = entry.clone();
                                rsx! {
                                    button {
                                        key: "{sc}",
                                        r#type: "button",
                                        class: "w-full text-left p-3 rounded border {row_cls} transition-colors",
                                        onclick: move |_| on_pick.call(entry_for_pick.clone()),
                                        div { class: "flex items-baseline gap-2",
                                            span { class: "font-semibold text-white",
                                                {entry.name.clone().unwrap_or_else(|| entry.shortcode.clone())}
                                            }
                                            span { class: "text-[10px] text-gray-500", "{entry.shortcode}" }
                                        }
                                        if let Some(d) = entry.description.clone() {
                                            div { class: "text-xs text-gray-400 mt-0.5", "{d}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            details { class: "text-xs text-gray-500",
                summary { class: "cursor-pointer hover:text-gray-300", "Rendezvous server (advanced)" }
                input {
                    class: "mt-2 w-full bg-[#1e1f22] border border-white/5 rounded-md px-3 py-1.5 text-xs text-white focus:outline-none focus:border-indigo-400",
                    r#type: "text",
                    placeholder: "ws://localhost:7700",
                    value: "{rendezvous_url}",
                    oninput: move |e| on_rendezvous_change.call(e.value()),
                }
            }
        }
    }
}

fn ws_to_http(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}
