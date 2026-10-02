//! Mounted at the workspace root, not in the voice panel: that panel sits in a
//! grid item, and a raised one traps `position: fixed` children (trap 22).

use std::time::{Duration, Instant};

use base64::Engine as _;
use dioxus::prelude::*;

use crate::features::voice::{VoiceCmd, VoiceTx, use_voice_tx};
use crate::protocol::{
    ClientMessage, GuildSound, Id, MAX_SOUND_BYTES, MAX_SOUND_NAME_LEN, MAX_SOUND_SECS,
    MAX_SOUNDS_PER_GUILD, Permission,
};
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

/// Under the gateway's limit of ten notices per ten seconds.
const PLAY_COOLDOWN: Duration = Duration::from_millis(1000);
const CHIP_SHOWN_FOR: Duration = Duration::from_secs(3);

#[component]
pub fn SoundboardPanel() -> Element {
    let mut state = use_app_state();
    let in_voice = use_memo(move || state.read().voice_guild().is_some());
    use_effect(move || {
        if !in_voice() && state.peek().soundboard_open {
            state.write().soundboard_open = false;
        }
    });

    let guild = state
        .read()
        .voice_guild()
        .filter(|_| state.read().soundboard_open);
    // A one-element list, so moving to another guild's call remounts it (trap 6).
    rsx! {
        for guild_id in guild {
            SoundboardPopover { key: "{guild_id}", guild_id }
        }
    }
}

const POPOVER_W: f64 = 280.0;

#[component]
fn SoundboardPopover(guild_id: Id) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();
    let mut last_play = use_signal(|| None::<Instant>);
    {
        let gw = gateway.clone();
        use_effect(move || crate::net::resolve_media(&mut state.write(), &gw.0));
    }
    let sounds = use_memo(move || state.read().sounds_of(guild_id).to_vec());
    let can_manage = state.read().can(guild_id, Permission::ManageGuild);
    let adjusting = state.read().soundboard_adjusting;
    let deafened = state.read().voice.deafened;
    let connected = state.read().voice.phase == VoicePhase::Connected;
    let blocked = if !connected {
        Some("Waiting for voice to connect…")
    } else if deafened {
        Some("You're deafened")
    } else {
        None
    };

    // Opens above the button when there is room, since the voice bar sits low.
    let (x, y) = state.read().soundboard_anchor;
    let left = x - POPOVER_W / 2.0;
    let place = if y > 240.0 {
        format!("top:{:.0}px; transform:translateY(-100%);", y - 8.0)
    } else {
        format!("top:{:.0}px;", y + 44.0)
    };
    let style = format!(
        "left:clamp(8px, {left:.0}px, calc(100vw - {:.0}px)); width:{POPOVER_W:.0}px; {place}",
        POPOVER_W + 8.0
    );

    rsx! {
        div {
            id: "dxf-soundboard",
            onpointerdown: move |e| e.stop_propagation(),
            class: "fixed z-[70] max-h-[50vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
            style: "{style}",
            oncontextmenu: move |e: MouseEvent| {
                e.prevent_default();
                state.write().soundboard_adjusting = true;
            },
            div { class: "h-8 pl-3 pr-1 flex items-center gap-2 border-b border-[var(--border)] shrink-0 select-none",
                span { class: "text-xs font-semibold text-[var(--text)] flex-1 truncate",
                    if adjusting { "Soundboard volume" } else { "Soundboard" }
                }
                button {
                    class: "[&>svg]:pointer-events-none w-6 h-6 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                    title: if adjusting { "Back to the sounds" } else { "Volume — or right-click anywhere here" },
                    onclick: move |_| {
                        let now = !state.read().soundboard_adjusting;
                        state.write().soundboard_adjusting = now;
                    },
                    dangerous_inner_html: if adjusting {
                        crate::features::icons::SOUNDBOARD
                    } else {
                        crate::features::icons::SPEAKER
                    },
                }
            }
            div { class: "p-2 overflow-y-auto",
                if adjusting {
                    SoundboardVolume {}
                    div { class: "mt-1 text-[10px] text-[var(--text-dim)]",
                        "How loud other people's sounds are for you, and yours to yourself."
                    }
                } else {
                    if sounds().is_empty() {
                        div { class: "px-1 py-0.5 text-[11px] text-[var(--text-dim)]",
                            if can_manage {
                                "No sounds yet — add them in guild settings, under Soundboard."
                            } else {
                                "This guild has no sounds yet."
                            }
                        }
                    }
                    div { class: "grid grid-cols-3 gap-1.5",
                        for sound in sounds().iter().cloned() {
                            {
                                let ready = blob_of(&state.read(), &sound).is_some();
                                let cooling = last_play().is_some_and(|t| t.elapsed() < PLAY_COOLDOWN);
                                let gw = gateway.clone();
                                let v = voice.clone();
                                let title = match blocked {
                                    Some(why) => why.to_string(),
                                    None if !ready => "Loading…".to_string(),
                                    None => sound.name.clone(),
                                };
                                rsx! {
                                    button {
                                        key: "{sound.id}",
                                        class: "h-9 px-2 rounded-md border border-[var(--border)] text-xs text-[var(--text)] truncate hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors disabled:opacity-40",
                                        disabled: !ready || blocked.is_some() || cooling,
                                        title: "{title}",
                                        onclick: move |_| {
                                            if last_play().is_some_and(|t| t.elapsed() < PLAY_COOLDOWN) {
                                                return;
                                            }
                                            last_play.set(Some(Instant::now()));
                                            play(state, gw.clone(), v.clone(), sound.clone());
                                        },
                                        "{sound.name}"
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

fn blob_of(s: &AppState, sound: &GuildSound) -> Option<(String, String)> {
    let address = sound.audio.strip_prefix("media:")?;
    let data_url = s.emoji_images.get(address).filter(|u| !u.is_empty())?;
    Some((address.to_string(), data_url.clone()))
}

fn play(mut state: Signal<AppState>, gateway: GatewayTx, voice: VoiceTx, sound: GuildSound) {
    let Some((address, data_url)) = blob_of(&state.peek(), &sound) else {
        return;
    };
    spawn(async move {
        let decoded =
            tokio::task::spawn_blocking(move || crate::sound_decode::decoded(&address, &data_url))
                .await;
        match decoded {
            Ok(Ok(pcm)) => {
                voice.send(VoiceCmd::PlaySound { pcm });
                gateway.send(ClientMessage::PlaySound { sound_id: sound.id });
                let mut s = state.write();
                if let Some(me) = s.self_user.as_ref().map(|u| u.pubkey.clone()) {
                    s.recent_sounds.insert(me, (sound.name, Instant::now()));
                }
            }
            Ok(Err(e)) => {
                state.write().error_toast = Some(format!("Couldn't play {}: {e}", sound.name));
            }
            Err(_) => {}
        }
    });
}

#[component]
pub fn SoundboardVolume() -> Element {
    let mut state = use_app_state();
    let voice = use_voice_tx();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let pct = state.read().soundboard_volume;
    rsx! {
        div { class: "flex items-center gap-2",
            input {
                r#type: "range",
                min: "0",
                max: "100",
                value: "{pct}",
                class: "flex-1 accent-[var(--accent)]",
                title: "Soundboard volume — only changes what you hear",
                oninput: move |e| {
                    let v: u32 = e.value().parse().unwrap_or(pct).min(100);
                    state.write().soundboard_volume = v;
                    settings.write().soundboard_volume = v as u8;
                    voice.send(VoiceCmd::SetSoundboardVolume { percent: v });
                },
                onchange: move |_| crate::settings::save(&settings.read()),
            }
            span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{pct}%" }
        }
    }
}

/// What someone just played, beside their name in the voice channel.
#[component]
pub fn SoundChip(pubkey: String) -> Element {
    let mut state = use_app_state();
    let recent = {
        let pk = pubkey.clone();
        use_memo(move || state.read().recent_sounds.get(&pk).cloned())
    };
    {
        let pk = pubkey.clone();
        use_effect(move || {
            let Some((_, at)) = recent() else { return };
            let pk = pk.clone();
            spawn(async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(at + CHIP_SHOWN_FOR)).await;
                let mut s = state.write();
                if s.recent_sounds.get(&pk).is_some_and(|(_, t)| *t == at) {
                    s.recent_sounds.remove(&pk);
                }
            });
        });
    }
    match recent().filter(|(_, at)| at.elapsed() < CHIP_SHOWN_FOR) {
        Some((name, _)) => rsx! {
            span {
                class: "text-[9px] text-[var(--accent)] truncate shrink-0 max-w-[40%]",
                title: "played {name}",
                "♪ {name}"
            }
        },
        None => rsx! {},
    }
}

#[derive(Clone, PartialEq)]
struct PendingSound {
    data_url: String,
    secs: f32,
}

/// Uploading is gated on Manage guild, not Manage emojis: a guild hands the
/// emoji grant out more freely, and a sound is played into every call.
#[component]
pub fn SoundSettings(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let sounds = use_memo(move || state.read().sounds_of(guild_id).to_vec());
    let mut name = use_signal(String::new);
    let mut pending = use_signal(|| None::<PendingSound>);
    let mut checking = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut renaming = use_signal(|| None::<Id>);
    let mut rename_draft = use_signal(String::new);

    let count = sounds().len();
    let full = count >= MAX_SOUNDS_PER_GUILD;
    let max_kb = MAX_SOUND_BYTES / 1024;

    let submit = {
        let gateway = gateway.clone();
        move |_| {
            let Some(sound) = pending() else {
                error.set(Some("Pick a sound file first.".into()));
                return;
            };
            let clean = match crate::protocol::sound_name(&name()) {
                Ok(n) => n,
                Err(_) => {
                    error.set(Some(format!(
                        "Name it in 1-{MAX_SOUND_NAME_LEN} characters."
                    )));
                    return;
                }
            };
            error.set(None);
            gateway.send(ClientMessage::CreateGuildSound {
                guild_id,
                name: clean,
                audio: sound.data_url,
            });
            name.set(String::new());
            pending.set(None);
        }
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3",
            div { class: "flex items-center gap-2 mb-1.5",
                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1",
                    "Soundboard"
                }
                div { class: "text-[10px] text-[var(--text-dim)]", "{count}/{MAX_SOUNDS_PER_GUILD}" }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                "Anyone in a voice channel here can play these to everyone in it. MP3, OGG or WAV, up to {max_kb} KB and {MAX_SOUND_SECS} seconds. Stored on this server."
            }

            if !full {
                div { class: "flex items-center gap-2 mb-2",
                    label {
                        class: "shrink-0 h-8 px-2 rounded border border-dashed border-[var(--border)] flex items-center justify-center cursor-pointer text-[10px] text-[var(--text-muted)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors",
                        title: "Choose an MP3, OGG or WAV file",
                        if checking() {
                            "Checking…"
                        } else if let Some(p) = pending() {
                            "{p.secs:.1} s"
                        } else {
                            "Choose file"
                        }
                        input {
                            r#type: "file",
                            accept: ".mp3,.ogg,.oga,.opus,.wav,audio/mpeg,audio/ogg,audio/opus,audio/wav",
                            class: "hidden",
                            onchange: move |evt: FormEvent| {
                                let files = evt.files();
                                spawn(async move {
                                    let Some(file) = files.into_iter().next() else { return };
                                    let file_name = file.name();
                                    let stem = file_name
                                        .rsplit_once('.')
                                        .map_or(file_name.as_str(), |(stem, _)| stem)
                                        .to_string();
                                    let bytes = match file.read_bytes().await {
                                        Ok(b) => b.to_vec(),
                                        Err(_) => {
                                            error.set(Some("Couldn't read that file.".into()));
                                            return;
                                        }
                                    };
                                    if bytes.len() > MAX_SOUND_BYTES {
                                        error.set(Some(format!("That file is over {max_kb} KB.")));
                                        return;
                                    }
                                    let Some((mime, ext)) = crate::sound_decode::sniff(&bytes) else {
                                        error.set(Some("That isn't an MP3, OGG or WAV file.".into()));
                                        return;
                                    };
                                    checking.set(true);
                                    let probe = bytes.clone();
                                    let decoded = tokio::task::spawn_blocking(move || {
                                        crate::sound_decode::decode(&probe, Some(ext))
                                    })
                                    .await;
                                    checking.set(false);
                                    let decoded = match decoded {
                                        Ok(Ok(d)) => d,
                                        Ok(Err(e)) => {
                                            error.set(Some(format!("Couldn't read that sound: {e}.")));
                                            return;
                                        }
                                        Err(_) => return,
                                    };
                                    if decoded.truncated {
                                        error.set(Some(format!(
                                            "That sound is longer than {MAX_SOUND_SECS} seconds. Trim it and try again."
                                        )));
                                        return;
                                    }
                                    let secs = decoded.pcm.len() as f32
                                        / crate::features::voice::SAMPLE_RATE as f32;
                                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    error.set(None);
                                    if name().trim().is_empty() {
                                        name.set(stem.chars().take(MAX_SOUND_NAME_LEN).collect());
                                    }
                                    pending.set(Some(PendingSound {
                                        data_url: format!("data:{mime};base64,{b64}"),
                                        secs,
                                    }));
                                });
                            },
                        }
                    }
                    input {
                        class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                        placeholder: "name",
                        maxlength: MAX_SOUND_NAME_LEN as i64,
                        value: "{name}",
                        oninput: move |e| name.set(e.value()),
                    }
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-40",
                        disabled: pending().is_none() || name().trim().is_empty(),
                        onclick: submit,
                        "Add"
                    }
                }
            } else {
                div { class: "text-[10px] text-[var(--warn)] mb-2",
                    "This guild has reached the sound limit. Remove one to add another."
                }
            }
            if let Some(e) = error() {
                div { class: "text-[10px] text-[var(--danger)] mb-2", "{e}" }
            }

            if sounds().is_empty() {
                div { class: "text-xs text-[var(--text-dim)]", "No sounds yet." }
            }
            for sound in sounds().iter().cloned() {
                {
                    let gw_del = gateway.clone();
                    let gw_ren = gateway.clone();
                    let id = sound.id;
                    let current = sound.name.clone();
                    let is_renaming = renaming() == Some(id);
                    let adder = state.read().display_name(&sound.added_by);
                    let provenance = crate::features::guild_settings::provenance(
                        &sound.added_by,
                        &adder,
                        sound.created_ms,
                    );
                    rsx! {
                        div { key: "{sound.id}", class: "flex items-center gap-2 py-1",
                            span { class: "block w-4 h-4 shrink-0 text-[var(--text-dim)]", dangerous_inner_html: crate::features::icons::SOUNDBOARD }
                            if is_renaming {
                                input {
                                    class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-2 py-0.5 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                                    maxlength: MAX_SOUND_NAME_LEN as i64,
                                    value: "{rename_draft}",
                                    oninput: move |ev| rename_draft.set(ev.value()),
                                }
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                                    onclick: move |_| {
                                        if let Ok(next) = crate::protocol::sound_name(&rename_draft()) {
                                            gw_ren.send(ClientMessage::RenameGuildSound {
                                                guild_id,
                                                sound_id: id,
                                                name: next,
                                            });
                                            renaming.set(None);
                                        }
                                    },
                                    "Save"
                                }
                            } else {
                                div { class: "flex-1 min-w-0",
                                    div { class: "text-xs text-[var(--text)] truncate", "{current}" }
                                    if let Some(line) = provenance {
                                        div {
                                            class: "text-[10px] text-[var(--text-dim)] truncate",
                                            title: "{sound.added_by}",
                                            "{line}"
                                        }
                                    }
                                }
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                                    onclick: move |_| {
                                        rename_draft.set(current.clone());
                                        renaming.set(Some(id));
                                    },
                                    "Rename"
                                }
                            }
                            button {
                                class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                onclick: move |_| gw_del.send(ClientMessage::DeleteGuildSound {
                                    guild_id,
                                    sound_id: id,
                                }),
                                "Remove"
                            }
                        }
                    }
                }
            }
        }
    }
}
