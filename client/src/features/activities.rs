use dioxus::prelude::*;
use rand::Rng;

use crate::protocol::{ClientMessage, Id};
use crate::state::{GatewayTx, use_app_state, use_gateway};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    UserRead,
    ChannelRead,
    MessageSend,
}

impl Capability {
    fn label(self) -> &'static str {
        match self {
            Capability::UserRead => "See your name and public key",
            Capability::ChannelRead => "See the current channel",
            Capability::MessageSend => "Post messages as you",
        }
    }
}

pub struct ActivityDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub caps: &'static [Capability],
}

pub const ACTIVITIES: &[ActivityDef] = &[ActivityDef {
    id: "dice",
    name: "Dice Roller",
    icon: "🎲",
    caps: &[
        Capability::UserRead,
        Capability::ChannelRead,
        Capability::MessageSend,
    ],
}];

#[component]
pub fn ActivityHost() -> Element {
    let state = use_app_state();

    let mut picker_open = use_signal(|| false);
    let mut consenting = use_signal(|| None::<usize>);
    let mut launched = use_signal(|| None::<Launched>);

    rsx! {
        button {
            class: "fixed bottom-3 left-3 z-40 border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            onclick: move |_| picker_open.set(!picker_open()),
            "Activities"
        }

        if picker_open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| { picker_open.set(false); consenting.set(None); },
                div {
                    class: "dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                        h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Activities" }
                        button {
                            class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                            onclick: move |_| { picker_open.set(false); consenting.set(None); },
                            "✕"
                        }
                    }
                    div { class: "p-2",
                        if let Some(idx) = consenting() {
                            ConsentPanel {
                                idx,
                                on_cancel: move |_| consenting.set(None),
                                on_approve: move |i: usize| {
                                    let channel = state.read().selected_channel;
                                    launched.set(Some(Launched { idx: i, channel }));
                                    consenting.set(None);
                                    picker_open.set(false);
                                },
                            }
                        } else {
                            for (i, def) in ACTIVITIES.iter().enumerate() {
                                button {
                                    key: "{def.id}",
                                    class: "w-full flex items-center gap-3 px-2 py-2 rounded hover:bg-white/[0.03] text-left transition-colors",
                                    onclick: move |_| consenting.set(Some(i)),
                                    span { class: "w-9 h-9 rounded-md border border-[var(--border)] flex items-center justify-center text-lg shrink-0",
                                        "{def.icon}"
                                    }
                                    div { class: "flex-1 min-w-0",
                                        div { class: "text-sm text-[var(--text)] truncate", "{def.name}" }
                                        div { class: "text-[10px] text-[var(--text-dim)]",
                                            "{def.caps.len()} permission(s)"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(idx) = launched().map(|l| l.idx) {
            if let Some(def) = ACTIVITIES.get(idx) {
                ActivityWindow { name: def.name, icon: def.icon, channel: launched().and_then(|l| l.channel),
                    on_close: move |_| launched.set(None),
                }
            }
        }
    }
}

#[component]
fn ConsentPanel(
    idx: usize,
    on_cancel: EventHandler<()>,
    on_approve: EventHandler<usize>,
) -> Element {
    let Some(def) = ACTIVITIES.get(idx) else {
        return rsx! { Fragment {} };
    };
    rsx! {
        div { class: "px-1 py-1",
            div { class: "flex items-center gap-2 mb-2",
                span { class: "text-lg", "{def.icon}" }
                span { class: "text-sm text-[var(--text)] font-medium", "{def.name}" }
            }
            div { class: "text-xs text-[var(--text-muted)] mb-1.5", "This activity will be able to:" }
            ul { class: "space-y-1 mb-3",
                for cap in def.caps.iter().copied() {
                    li { class: "flex items-start gap-2 text-xs text-[var(--text)]",
                        span { class: "text-[var(--accent)]", "•" }
                        span { "{cap.label()}" }
                    }
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                "Runs locally in your client using the permissions listed above."
            }
            div { class: "flex gap-2",
                button {
                    class: "flex-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| on_approve.call(idx),
                    "Launch"
                }
                button {
                    class: "rounded px-3 py-1.5 text-[11px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Move { dx: f64, dy: f64 },
    Resize { px: f64, py: f64, w0: f64, h0: f64 },
}

#[component]
fn ActivityWindow(
    name: &'static str,
    icon: &'static str,
    channel: Option<Id>,
    on_close: EventHandler<()>,
) -> Element {
    let mut x = use_signal(|| 220.0_f64);
    let mut y = use_signal(|| 120.0_f64);
    let mut w = use_signal(|| 420.0_f64);
    let mut h = use_signal(|| 460.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    rsx! {
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { x.set(c.x - dx); y.set(c.y - dy); }
                        Some(Drag::Resize { px, py, w0, h0 }) => {
                            w.set((w0 + (c.x - px)).max(300.0));
                            h.set((h0 + (c.y - py)).max(260.0));
                        }
                        None => {}
                    }
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            class: "fixed z-40 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-2xl overflow-hidden dxf-modal-in",
            style: "left: {x}px; top: {y}px; width: {w}px; height: {h}px;",
            div {
                class: "h-9 px-3 flex items-center gap-2 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - x(), dy: c.y - y() }));
                },
                span { class: "text-sm", "{icon}" }
                span { class: "text-sm text-[var(--text)] font-medium truncate flex-1", "{name}" }
                span { class: "text-[9px] uppercase tracking-wider text-[var(--text-dim)]", "Local" }
                button {
                    class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none ml-1",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| on_close.call(()),
                    "✕"
                }
            }
            DicePanel { channel }
            div {
                class: "absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize",
                style: "background: linear-gradient(135deg, transparent 0 50%, var(--border-strong) 50% 100%);",
                onmousedown: move |e| {
                    e.stop_propagation();
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: w(), h0: h() }));
                },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Launched {
    idx: usize,
    channel: Option<Id>,
}

#[component]
fn DicePanel(channel: Option<Id>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut last = use_signal(|| None::<u8>);
    let mut rolling = use_signal(|| false);
    let mut shared = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    let mut error = use_signal(|| None::<String>);
    let snapshot = state.read();
    let username = snapshot
        .self_user
        .as_ref()
        .map(|u| snapshot.display_name(&u.pubkey));
    let channel_name = channel.and_then(|id| {
        snapshot
            .channels
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
    });
    drop(snapshot);
    let face = last()
        .map(|n| ["⚀", "⚁", "⚂", "⚃", "⚄", "⚅"][usize::from(n - 1)])
        .unwrap_or("🎲");
    rsx! {
        div { class: "flex-1 min-h-0 flex flex-col items-center justify-center gap-4",
            if let Some(name) = username { div { class: "text-sm text-[var(--text-muted)]", "Hi, {name}" } }
            div { style: if rolling() { "font-size:84px;transform:rotate(20deg) scale(1.1);transition:transform .15s;" } else { "font-size:84px;transition:transform .15s;" }, "{face}" }
            div { class: "flex gap-2",
                button { class: "rounded border border-[var(--border)] px-3 py-2 text-[var(--accent)]", disabled: rolling(),
                    onclick: move |_| {
                        let value = rand::thread_rng().gen_range(1..=6);
                        rolling.set(true);
                        shared.set(false);
                        generation += 1;
                        error.set(None);
                        spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                            last.set(Some(value));
                            rolling.set(false);
                        });
                    }, "Roll"
                }
                button { class: "rounded border border-[var(--border)] px-3 py-2 text-[var(--accent)]", disabled: rolling() || last().is_none() || shared() || channel.is_none(),
                    onclick: move |_| {
                        if let (Some(channel_id), Some(value)) = (channel, last()) {
                            let content = format!("🎲 rolled a {value}!");
                            match send_dice(&gateway, channel_id, content) {
                                Ok(()) => {
                                    shared.set(true);
                                    generation += 1;
                                    let request = generation();
                                    spawn(async move {
                                        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                                        if generation() == request { shared.set(false); }
                                    });
                                },
                                Err(message) => error.set(Some(message)),
                            }
                        }
                    },
                    if shared() { "Shared!" } else { "Share to chat" }
                }
            }
            if let Some(name) = channel_name { div { class: "text-xs text-[var(--text-muted)]", "shares go to #{name}" } }
            if let Some(message) = error() { div { class: "text-xs text-[var(--danger)]", "{message}" } }
        }
    }
}

fn send_dice(gateway: &GatewayTx, channel_id: Id, content: String) -> Result<(), String> {
    gateway
        .0
        .send(ClientMessage::SendMessage {
            channel_id,
            content,
            image: None,
            reply_to: None,
        })
        .map_err(|_| "The connection is closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_roll_uses_its_bound_channel_and_reports_a_closed_connection() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = GatewayTx(tx);
        let channel = Id::new_v4();
        send_dice(&gateway, channel, "🎲 rolled a 6!".into()).unwrap();
        match rx.try_recv().unwrap() {
            ClientMessage::SendMessage {
                channel_id,
                content,
                image,
                reply_to,
            } => {
                assert_eq!(channel_id, channel);
                assert_eq!(content, "🎲 rolled a 6!");
                assert!(image.is_none() && reply_to.is_none());
            }
            message => panic!("unexpected message: {message:?}"),
        }
        drop(rx);
        assert!(send_dice(&gateway, channel, "🎲 rolled a 1!".into()).is_err());
    }
}
