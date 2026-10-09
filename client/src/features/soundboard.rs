//! Mounted at the workspace root, not in the voice panel: that panel sits in a
//! grid item, and a raised one traps `position: fixed` children (trap 22).

use std::sync::Arc;
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
/// Under the gateway's thirty writes per ten seconds, with room for the
/// person's other commands.
const UPLOAD_SPACING: Duration = Duration::from_millis(400);
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
                aria_label: "Soundboard playback volume",
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

/// Only to this machine, through the app-sound output, at the soundboard
/// volume: what everyone else would hear, without telling the server.
fn preview(state: &AppState, pcm: Arc<[f32]>) {
    let gain = state.soundboard_volume.min(200) as f32 / 100.0;
    crate::native_sounds::play_clip(pcm, crate::features::voice::SAMPLE_RATE, gain);
}

fn preview_sound(mut state: Signal<AppState>, sound: GuildSound) {
    let Some((address, data_url)) = blob_of(&state.peek(), &sound) else {
        return;
    };
    spawn(async move {
        let decoded =
            tokio::task::spawn_blocking(move || crate::sound_decode::decoded(&address, &data_url))
                .await;
        match decoded {
            Ok(Ok(pcm)) => preview(&state.peek(), pcm),
            Ok(Err(e)) => {
                state.write().error_toast = Some(format!("Couldn't play {}: {e}", sound.name));
            }
            Err(_) => {}
        }
    });
}

#[derive(Clone)]
struct PendingSound {
    key: u64,
    name: String,
    data_url: String,
    secs: f32,
    pcm: Arc<[f32]>,
}

/// Everything the uploader must know about a file before offering it.
fn checked(bytes: Vec<u8>) -> Result<(String, f32, Arc<[f32]>), String> {
    let max_kb = MAX_SOUND_BYTES / 1024;
    if bytes.len() > MAX_SOUND_BYTES {
        return Err(format!("over {max_kb} KB"));
    }
    let Some((mime, ext)) = crate::sound_decode::sniff(&bytes) else {
        return Err("not an MP3, OGG or WAV file".into());
    };
    let decoded = crate::sound_decode::decode(&bytes, Some(ext))
        .map_err(|e| format!("couldn't read it: {e}"))?;
    if decoded.truncated {
        return Err(format!("longer than {MAX_SOUND_SECS} seconds"));
    }
    let secs = decoded.pcm.len() as f32 / crate::features::voice::SAMPLE_RATE as f32;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok((
        format!("data:{mime};base64,{b64}"),
        secs,
        decoded.pcm.into(),
    ))
}

fn stem_of(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem)
        .chars()
        .take(MAX_SOUND_NAME_LEN)
        .collect()
}

/// Uploading is gated on Manage guild, not Manage emojis: a guild hands the
/// emoji grant out more freely, and a sound is played into every call.
#[component]
pub fn SoundSettings(guild_id: Id) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    {
        let gw = gateway.clone();
        use_effect(move || crate::net::resolve_media(&mut state.write(), &gw.0));
    }

    let sounds = use_memo(move || state.read().sounds_of(guild_id).to_vec());
    let mut pending = use_signal(Vec::<PendingSound>::new);
    let mut next_key = use_signal(|| 0_u64);
    let mut checking = use_signal(|| 0_usize);
    let mut adding = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut renaming = use_signal(|| None::<Id>);
    let mut rename_draft = use_signal(String::new);

    let count = sounds().len();
    let room = MAX_SOUNDS_PER_GUILD.saturating_sub(count);
    let full = room == 0;
    let max_kb = MAX_SOUND_BYTES / 1024;
    let queued = pending().len();
    let names_ok = pending()
        .iter()
        .all(|p| crate::protocol::sound_name(&p.name).is_ok());

    let submit_all = {
        let gateway = gateway.clone();
        move |_| {
            let mut sends = Vec::new();
            for p in pending().iter() {
                match crate::protocol::sound_name(&p.name) {
                    Ok(n) => sends.push((p.key, n, p.data_url.clone())),
                    Err(_) => {
                        error.set(Some(format!(
                            "Name every sound in 1-{MAX_SOUND_NAME_LEN} characters."
                        )));
                        return;
                    }
                }
            }
            if sends.is_empty() {
                return;
            }
            if sends.len() > room {
                error.set(Some(format!("Only {room} more can be added here.")));
                return;
            }
            error.set(None);
            adding.set(true);
            let gw = gateway.clone();
            spawn(async move {
                for (i, (key, name, audio)) in sends.into_iter().enumerate() {
                    if i > 0 {
                        tokio::time::sleep(UPLOAD_SPACING).await;
                    }
                    gw.send(ClientMessage::CreateGuildSound {
                        guild_id,
                        name,
                        audio,
                    });
                    pending.write().retain(|p| p.key != key);
                }
                adding.set(false);
            });
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
                "Anyone in a voice channel here can play these to everyone in it. MP3, OGG or WAV, up to {max_kb} KB and {MAX_SOUND_SECS} seconds each. Stored on this server."
            }

            if !full {
                div { class: "flex items-center gap-2 mb-2",
                    label {
                        class: "shrink-0 h-8 px-3 rounded border border-dashed border-[var(--border)] flex items-center justify-center cursor-pointer text-[10px] text-[var(--text-muted)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors",
                        title: "Choose one or more MP3, OGG or WAV files",
                        if checking() > 0 {
                            "Checking {checking()}…"
                        } else {
                            "Choose files"
                        }
                        input {
                            r#type: "file",
                            multiple: true,
                            accept: ".mp3,.ogg,.oga,.opus,.wav,audio/mpeg,audio/ogg,audio/opus,audio/wav",
                            class: "hidden",
                            onchange: move |evt: FormEvent| {
                                let files = evt.files();
                                spawn(async move {
                                    let mut skipped: Vec<String> = Vec::new();
                                    for file in files {
                                        let file_name = file.name();
                                        let room_left = MAX_SOUNDS_PER_GUILD
                                            .saturating_sub(state.peek().sounds_of(guild_id).len())
                                            .saturating_sub(pending.peek().len());
                                        if room_left == 0 {
                                            skipped.push(format!("{file_name}: no room left"));
                                            continue;
                                        }
                                        let bytes = match file.read_bytes().await {
                                            Ok(b) => b.to_vec(),
                                            Err(_) => {
                                                skipped.push(format!("{file_name}: couldn't read it"));
                                                continue;
                                            }
                                        };
                                        checking.with_mut(|n| *n += 1);
                                        let result =
                                            tokio::task::spawn_blocking(move || checked(bytes)).await;
                                        checking.with_mut(|n| *n = n.saturating_sub(1));
                                        match result {
                                            Ok(Ok((data_url, secs, pcm))) => {
                                                let key = next_key();
                                                next_key.set(key + 1);
                                                pending.write().push(PendingSound {
                                                    key,
                                                    name: stem_of(&file_name),
                                                    data_url,
                                                    secs,
                                                    pcm,
                                                });
                                            }
                                            Ok(Err(why)) => skipped.push(format!("{file_name}: {why}")),
                                            Err(_) => {}
                                        }
                                    }
                                    error.set((!skipped.is_empty())
                                        .then(|| format!("Skipped {}.", skipped.join("; "))));
                                });
                            },
                        }
                    }
                    div { class: "text-[10px] text-[var(--text-dim)] flex-1 min-w-0 truncate",
                        if queued == 0 {
                            "Pick several at once; each gets a name and a preview before it is added."
                        } else {
                            "{queued} ready to add"
                        }
                    }
                }
            } else {
                div { class: "text-[10px] text-[var(--warn)] mb-2",
                    "This guild has reached the sound limit. Remove one to add another."
                }
            }

            if queued > 0 {
                div { class: "mb-2 rounded border border-[var(--border)] px-2 py-1",
                    for p in pending().iter().cloned() {
                        {
                            let key = p.key;
                            let pcm = p.pcm.clone();
                            let name_ok = crate::protocol::sound_name(&p.name).is_ok();
                            rsx! {
                                div { key: "{key}", class: "flex items-center gap-2 py-1",
                                    button {
                                        class: "w-6 h-6 shrink-0 flex items-center justify-center rounded text-[var(--accent)] hover:bg-white/[0.06] transition-colors [&>svg]:w-3.5 [&>svg]:h-3.5",
                                        title: "Preview — only you hear it",
                                        onclick: move |_| preview(&state.peek(), pcm.clone()),
                                        dangerous_inner_html: crate::features::icons::PLAY,
                                    }
                                    input {
                                        class: if name_ok {
                                            "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-2 py-0.5 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]"
                                        } else {
                                            "flex-1 min-w-0 bg-transparent border border-[var(--danger)] rounded px-2 py-0.5 text-xs text-[var(--text)] focus:outline-none"
                                        },
                                        placeholder: "name",
                                        maxlength: MAX_SOUND_NAME_LEN as i64,
                                        value: "{p.name}",
                                        oninput: move |e| {
                                            if let Some(entry) = pending.write().iter_mut().find(|q| q.key == key) {
                                                entry.name = e.value();
                                            }
                                        },
                                    }
                                    span { class: "text-[10px] text-[var(--text-dim)] shrink-0 w-10 text-right", "{p.secs:.1} s" }
                                    button {
                                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors shrink-0",
                                        title: "Don't add this one",
                                        onclick: move |_| pending.write().retain(|q| q.key != key),
                                        "✕"
                                    }
                                }
                            }
                        }
                    }
                    div { class: "flex items-center gap-2 py-1",
                        button {
                            class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-40",
                            disabled: adding() || !names_ok || queued > room,
                            onclick: submit_all,
                            if adding() {
                                "Adding…"
                            } else if queued == 1 {
                                "Add 1 sound"
                            } else {
                                "Add {queued} sounds"
                            }
                        }
                        button {
                            class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] transition-colors disabled:opacity-40",
                            disabled: adding(),
                            onclick: move |_| pending.set(Vec::new()),
                            "Clear"
                        }
                    }
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
                    let ready = blob_of(&state.read(), &sound).is_some();
                    let adder = state.read().display_name(&sound.added_by);
                    let provenance = crate::features::guild_settings::provenance(
                        &sound.added_by,
                        &adder,
                        sound.created_ms,
                    );
                    let to_preview = sound.clone();
                    rsx! {
                        div { key: "{sound.id}", class: "flex items-center gap-2 py-1",
                            button {
                                class: "w-6 h-6 shrink-0 flex items-center justify-center rounded text-[var(--accent)] hover:bg-white/[0.06] transition-colors disabled:opacity-40 [&>svg]:w-3.5 [&>svg]:h-3.5",
                                title: if ready { "Preview — only you hear it" } else { "Loading…" },
                                disabled: !ready,
                                onclick: move |_| preview_sound(state, to_preview.clone()),
                                dangerous_inner_html: crate::features::icons::PLAY,
                            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_named_by_its_stem_within_the_limit() {
        assert_eq!(stem_of("airhorn.mp3"), "airhorn");
        assert_eq!(stem_of("no_extension"), "no_extension");
        assert_eq!(
            stem_of(&format!("{}.wav", "x".repeat(40))).len(),
            MAX_SOUND_NAME_LEN
        );
    }

    #[test]
    fn checked_rejects_what_the_server_would_store_blindly() {
        assert!(
            checked(vec![0; MAX_SOUND_BYTES + 1])
                .unwrap_err()
                .contains("KB")
        );
        assert!(
            checked(b"not audio at all".to_vec())
                .unwrap_err()
                .contains("MP3")
        );
    }
}
