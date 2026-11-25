use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike, Utc};
use dioxus::prelude::*;

use crate::protocol::{
    ChannelKind, ClientMessage, EventDraft, EventRepeat, GuildEvent, Id, Permission,
};
use crate::state::{use_app_state, use_gateway};

/// The guild whose events dialog is open. Mounted at the workspace root, not in
/// the chat panel, whose stacking context would trap it (trap 22).
#[derive(Clone, Copy)]
pub struct EventsTarget(pub Signal<Option<Id>>);

const LIVE_NAME_CHARS: usize = 24;
const FIELD: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const SECONDARY: &str = "shrink-0 rounded-lg px-3 py-1 text-xs text-[var(--text-muted)] border border-[var(--border)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors";

const REPEATS: &[(EventRepeat, &str)] = &[
    (EventRepeat::Never, "Does not repeat"),
    (EventRepeat::Daily, "Every day"),
    (EventRepeat::Weekly, "Every week"),
    (EventRepeat::Monthly, "Every month"),
    (EventRepeat::Yearly, "Every year"),
];

/// Asks once per guild and session: clearing the session empties the map, so a
/// reconnect asks again. The empty list stands for a question already asked.
pub fn use_guild_events(guild_id: Option<Id>) {
    let mut state = use_app_state();
    let gateway = use_gateway();
    use_effect(use_reactive((&guild_id,), move |(guild_id,)| {
        let Some(gid) = guild_id else { return };
        let s = state.read();
        if !s.server_events || s.guild_events.contains_key(&gid) {
            return;
        }
        drop(s);
        state.write().guild_events.insert(gid, Vec::new());
        gateway.send(ClientMessage::FetchGuildEvents { guild_id: gid });
    }));
}

/// Re-renders every 30 s, so live state changes without a message.
fn use_clock() -> DateTime<Utc> {
    let mut tick = use_signal(|| 0u64);
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            tick += 1;
        }
    });
    let _ = tick();
    Utc::now()
}

fn shortened(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(chars.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn local(at: DateTime<Utc>, format: &str) -> String {
    at.with_timezone(&Local).format(format).to_string()
}

fn local_parts(at: DateTime<Utc>) -> (NaiveDate, (u32, u32)) {
    let at = at.with_timezone(&Local);
    (at.date_naive(), (at.hour(), at.minute()))
}

fn local_instant(date: NaiveDate, (hour, minute): (u32, u32)) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(hour, minute, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// An all-day span: local midnight of its first day to that of the day after its last.
fn day_span(first: NaiveDate, last: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    Some((
        local_instant(first, (0, 0))?,
        local_instant(last.succ_opt()?, (0, 0))?,
    ))
}

/// The last day an all-day event covers: its end is the next day's midnight.
fn last_day(ends_at: DateTime<Utc>) -> NaiveDate {
    local_parts(ends_at - chrono::Duration::seconds(1)).0
}

fn shown_when(event: &GuildEvent, start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let first = local_parts(start).0;
    if event.all_day {
        let last = last_day(end);
        let days = if last > first {
            format!(
                "{} – {}",
                super::date_picker::shown_date(first),
                super::date_picker::shown_date(last)
            )
        } else {
            super::date_picker::shown_date(first)
        };
        crate::i18n::text("All day · {0}", &[days])
    } else {
        format!(
            "{} · {}",
            super::date_picker::shown_date(first),
            local(start, "%H:%M")
        )
    }
}

#[component]
pub fn LiveEventBadge(guild_id: Id) -> Element {
    use_guild_events(Some(guild_id));
    let state = use_app_state();
    let now = use_clock();
    let live = state
        .read()
        .guild_events
        .get(&guild_id)
        .and_then(|events| events.iter().find(|e| e.is_live(now)).cloned());
    let Some(event) = live else {
        return rsx! {};
    };
    let short = shortened(&event.name, LIVE_NAME_CHARS);
    rsx! {
        span {
            class: "dxf-live-event min-w-0 max-w-[11rem] shrink flex items-center gap-1 px-1.5 h-5 rounded-full bg-[var(--accent-soft)] text-[10.5px] font-medium text-[var(--accent)]",
            title: crate::i18n::text("Happening now: {0}", std::slice::from_ref(&event.name)),
            span { class: "w-1.5 h-1.5 shrink-0 rounded-full bg-[var(--accent)] animate-pulse" }
            span { class: "truncate", "{short}" }
        }
    }
}

#[component]
pub fn EventsButton(guild_id: Id) -> Element {
    use_guild_events(Some(guild_id));
    let state = use_app_state();
    let now = use_clock();
    let target = try_use_context::<EventsTarget>();
    let s = state.read();
    if !s.server_events {
        return rsx! {};
    }
    let events = s.guild_events.get(&guild_id);
    let upcoming = events.map_or(0, |e| {
        e.iter().filter(|e| e.occurrence(now).is_some()).count()
    });
    let live = events.is_some_and(|e| e.iter().any(|e| e.is_live(now)));
    drop(s);
    let Some(EventsTarget(mut open)) = target else {
        return rsx! {};
    };
    rsx! {
        button {
            r#type: "button",
            class: "relative h-8 px-2.5 flex items-center gap-2 shrink-0 rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[11px] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
            "data-tip": crate::i18n::text("This community's events", &[]),
            onclick: move |_| open.set(Some(guild_id)),
            if live {
                span { class: "w-2 h-2 rounded-full bg-[var(--accent)] animate-pulse" }
            }
            span { class: "block w-3.5 h-3.5", dangerous_inner_html: super::icons::CALENDAR }
            {crate::i18n::text("Events", &[])}
            if upcoming > 0 {
                span { class: "min-w-5 h-5 px-1.5 flex items-center justify-center rounded-md bg-[var(--accent-soft)] text-[10px] text-[var(--accent)]",
                    "{upcoming}"
                }
            }
        }
    }
}

#[component]
pub fn EventsDialog() -> Element {
    let Some(EventsTarget(mut target)) = try_use_context::<EventsTarget>() else {
        return rsx! {};
    };
    let Some(guild_id) = target() else {
        return rsx! {};
    };
    rsx! {
        EventsPanel { key: "{guild_id}", guild_id, on_close: move |_| target.set(None) }
    }
}

#[derive(Clone, PartialEq)]
enum Pane {
    List,
    Edit(Option<GuildEvent>),
}

fn host_label(state: &crate::state::AppState, channel_id: Option<Id>) -> Option<String> {
    let channel = state.channels.iter().find(|c| Some(c.id) == channel_id)?;
    Some(match channel.kind {
        ChannelKind::Voice => format!("🔊 {}", channel.name),
        _ => format!("# {}", channel.name),
    })
}

fn repeat_label(repeat: EventRepeat) -> String {
    let label = REPEATS
        .iter()
        .find(|(r, _)| *r == repeat)
        .map_or("Does not repeat", |(_, l)| l);
    crate::i18n::label(label).to_string()
}

/// An event, its current or next run, its host and its cover, as listed.
type EventRow = (
    GuildEvent,
    Option<(DateTime<Utc>, DateTime<Utc>)>,
    Option<String>,
    Option<String>,
);

#[component]
fn EventsPanel(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let now = use_clock();
    let mut pane = use_signal(|| Pane::List);
    let mut confirm_delete = use_signal(|| None::<Id>);

    let s = state.read();
    let can_manage = s.can(guild_id, Permission::ManageEvents);
    let mut events: Vec<EventRow> = s
        .guild_events
        .get(&guild_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            let run = e.occurrence(now);
            let host = host_label(&s, e.channel_id);
            let cover = e
                .cover
                .as_deref()
                .and_then(|c| crate::image_assets::media_uri(&s, c));
            (e, run, host, cover)
        })
        .collect();
    drop(s);
    events.sort_by_key(|(e, run, ..)| (run.is_none(), run.map_or(e.starts_at, |r| r.0)));

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                style: "width: min(30rem, calc(100vw - 2rem)); max-height: calc(100vh - 2rem);",
                role: "dialog",
                aria_label: crate::i18n::text("Events", &[]),
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center gap-2",
                    h3 { class: "flex-1 text-sm font-medium text-[var(--text)]",
                        {match pane() {
                            Pane::List => crate::i18n::text("Events", &[]),
                            Pane::Edit(None) => crate::i18n::text("New event", &[]),
                            Pane::Edit(Some(_)) => crate::i18n::text("Edit event", &[]),
                        }}
                    }
                    button {
                        r#type: "button",
                        class: "w-7 h-7 flex items-center justify-center rounded-md text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                        title: crate::i18n::text("Close", &[]),
                        onclick: move |_| on_close.call(()),
                        span { class: "block w-4 h-4", dangerous_inner_html: super::icons::CLOSE }
                    }
                }
                {match pane() {
                    Pane::Edit(event) => rsx! {
                        EventForm { guild_id, event, on_done: move |_| pane.set(Pane::List) }
                    },
                    Pane::List => rsx! {
                        div { class: "px-4 py-3 flex flex-col gap-3 overflow-y-auto",
                            if events.is_empty() {
                                div { class: "py-6 text-center text-xs text-[var(--text-dim)]",
                                    {crate::i18n::text("No events scheduled.", &[])}
                                }
                            }
                            for (event, run, host, cover) in events {
                                {
                                    let id = event.id;
                                    let when = match run {
                                        Some((start, _)) if start <= now && event.all_day => crate::i18n::text("Happening now · all day", &[]),
                                        Some((start, end)) if start <= now => crate::i18n::text("Happening now · until {0}", &[local(end, "%H:%M")]),
                                        Some((start, end)) => shown_when(&event, start, end),
                                        None => crate::i18n::text("Ended", &[]),
                                    };
                                    let live = run.is_some_and(|(start, _)| start <= now);
                                    let editing = event.clone();
                                    let gateway = gateway.clone();
                                    rsx! {
                                        div { key: "{id}", class: "flex flex-col overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--panel2)]",
                                            if let Some(src) = cover {
                                                img { class: "w-full h-24 object-cover block", src: "{src}", alt: "" }
                                            }
                                            div { class: "px-3 py-2.5 flex flex-col gap-1",
                                                div { class: "text-sm font-semibold text-[var(--text)] break-words", "{event.name}" }
                                                div {
                                                    class: if live { "text-[11px] font-medium text-[var(--accent)]" } else { "text-[11px] text-[var(--text-muted)]" },
                                                    "{when}"
                                                }
                                                div { class: "flex flex-wrap gap-x-2 text-[10.5px] text-[var(--text-dim)]",
                                                    if event.repeat != EventRepeat::Never {
                                                        span { {repeat_label(event.repeat)} }
                                                    }
                                                    if let Some(host) = host {
                                                        span { "{host}" }
                                                    }
                                                }
                                                if !event.description.is_empty() {
                                                    div { class: "text-xs text-[var(--text-muted)] whitespace-pre-wrap break-words", "{event.description}" }
                                                }
                                                if can_manage {
                                                    div { class: "flex items-center justify-end gap-2 pt-1",
                                                        if confirm_delete() == Some(id) {
                                                            span { class: "flex-1 text-[11px] text-[var(--danger)]", {crate::i18n::text("Delete this event?", &[])} }
                                                            button { r#type: "button", class: SECONDARY, onclick: move |_| confirm_delete.set(None), {crate::i18n::text("Cancel", &[])} }
                                                            button {
                                                                r#type: "button",
                                                                class: "shrink-0 rounded-lg px-3 py-1 text-xs text-[var(--danger)] border border-[var(--danger)] transition-colors",
                                                                onclick: move |_| {
                                                                    gateway.send(ClientMessage::DeleteGuildEvent { guild_id, event_id: id });
                                                                    confirm_delete.set(None);
                                                                },
                                                                {crate::i18n::text("Delete", &[])}
                                                            }
                                                        } else {
                                                            button { r#type: "button", class: SECONDARY, onclick: move |_| confirm_delete.set(Some(id)), {crate::i18n::text("Delete", &[])} }
                                                            button { r#type: "button", class: SECONDARY, onclick: move |_| pane.set(Pane::Edit(Some(editing.clone()))), {crate::i18n::text("Edit", &[])} }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if can_manage {
                            div { class: "px-4 py-3 border-t border-[var(--border)] flex justify-end",
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1 rounded border border-[var(--accent)] text-[11px] text-[var(--accent)] hover:bg-[var(--accent-soft)] transition-colors",
                                    onclick: move |_| pane.set(Pane::Edit(None)),
                                    {crate::i18n::text("New event", &[])}
                                }
                            }
                        }
                    },
                }}
            }
        }
    }
}

#[component]
fn EventForm(guild_id: Id, event: Option<GuildEvent>, on_done: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let editing = event.as_ref().map(|e| e.id);
    let (start_default, end_default) = match &event {
        Some(e) => (e.starts_at, e.ends_at),
        None => {
            let next = Local::now() + chrono::Duration::hours(1);
            let start = next
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(next)
                .with_timezone(&Utc);
            (start, start + chrono::Duration::hours(1))
        }
    };
    let mut name = use_signal(|| event.as_ref().map(|e| e.name.clone()).unwrap_or_default());
    let mut description = use_signal(|| {
        event
            .as_ref()
            .map(|e| e.description.clone())
            .unwrap_or_default()
    });
    let mut all_day = use_signal(|| event.as_ref().is_some_and(|e| e.all_day));
    let mut start_date = use_signal(|| local_parts(start_default).0);
    let mut start_time = use_signal(|| local_parts(start_default).1);
    let mut end_date = use_signal(|| {
        if event.as_ref().is_some_and(|e| e.all_day) {
            last_day(end_default)
        } else {
            local_parts(end_default).0
        }
    });
    let mut end_time = use_signal(|| local_parts(end_default).1);
    let mut repeat = use_signal(|| event.as_ref().map_or(EventRepeat::Never, |e| e.repeat));
    let mut host = use_signal(|| event.as_ref().and_then(|e| e.channel_id));
    let mut cover = use_signal(|| event.as_ref().and_then(|e| e.cover.clone()));
    let mut error = use_signal(|| None::<String>);

    let s = state.read();
    let hosts: Vec<(Id, String)> = s
        .channels
        .iter()
        .filter(|c| {
            c.guild_id == guild_id && matches!(c.kind, ChannelKind::Text | ChannelKind::Voice)
        })
        .map(|c| (c.id, host_label(&s, Some(c.id)).unwrap_or_default()))
        .collect();
    let cover_limit = s
        .guild_file_policies
        .get(&guild_id)
        .map_or(crate::protocol::DEFAULT_GUILD_AVATAR_BYTES, |p| {
            p.max_avatar_bytes
        }) as usize;
    let cover_src = cover().and_then(|c| {
        if c.starts_with("data:") {
            Some(c)
        } else {
            crate::image_assets::media_uri(&s, &c)
        }
    });
    drop(s);

    let save = move |_| {
        let span = if all_day() {
            day_span(start_date(), end_date())
        } else {
            local_instant(start_date(), start_time()).zip(local_instant(end_date(), end_time()))
        };
        let Some((starts_at, ends_at)) = span else {
            error.set(Some(crate::i18n::text(
                "Pick a start and an end time.",
                &[],
            )));
            return;
        };
        let draft = EventDraft {
            name: name(),
            description: description(),
            starts_at,
            ends_at,
            channel_id: host(),
            repeat: repeat(),
            cover: cover(),
            all_day: all_day(),
        };
        match draft.validated() {
            Err(message) => error.set(Some(message)),
            Ok(draft) => {
                gateway.send(ClientMessage::SaveGuildEvent {
                    guild_id,
                    event_id: editing,
                    draft,
                });
                on_done.call(());
            }
        }
    };
    let pick_cover = move |_| {
        spawn(async move {
            match crate::file_dialog::pick_file(&[(
                crate::i18n::label("Image"),
                crate::file_dialog::STORED_IMAGE_EXTENSIONS,
            )])
            .await
            {
                Ok(Some(path)) => match tokio::task::spawn_blocking(move || {
                    crate::chat_image::read_file_with_limit(&path, cover_limit)
                })
                .await
                {
                    Ok(Ok(image)) => {
                        cover.set(Some(image));
                        error.set(None);
                    }
                    Ok(Err(e)) => error.set(Some(e)),
                    Err(e) => error.set(Some(e.to_string())),
                },
                Ok(None) => {}
                Err(message) => error.set(Some(message)),
            }
        });
    };

    rsx! {
        div { class: "px-4 py-3 flex flex-col gap-3 overflow-y-auto",
            input {
                class: FIELD,
                r#type: "text",
                maxlength: "{crate::protocol::EVENT_NAME_CHARS}",
                placeholder: crate::i18n::text("Event name", &[]),
                value: "{name}",
                autofocus: true,
                oninput: move |e| { name.set(e.value()); error.set(None); },
            }
            textarea {
                class: "{FIELD} min-h-[4.5rem] resize-y",
                maxlength: "{crate::protocol::EVENT_DESCRIPTION_CHARS}",
                placeholder: crate::i18n::text("What's happening? (optional)", &[]),
                value: "{description}",
                oninput: move |e| description.set(e.value()),
            }
            label { class: "flex items-center gap-2 cursor-pointer select-none text-xs text-[var(--text)]",
                input {
                    r#type: "checkbox",
                    class: "accent-[var(--accent)]",
                    checked: all_day(),
                    onchange: move |e| { all_day.set(e.checked()); error.set(None); },
                }
                {crate::i18n::text("All day", &[])}
            }
            div { class: "flex flex-col gap-1",
                span { class: "text-[11px] text-[var(--text-muted)]", {crate::i18n::text("Starts", &[])} }
                div { class: if all_day() { "grid grid-cols-1" } else { "grid grid-cols-[1fr_6.5rem] gap-2 items-start" },
                    super::date_picker::DatePicker {
                        value: start_date(),
                        label: crate::i18n::text("Starts", &[]),
                        on_change: move |date: NaiveDate| {
                            start_date.set(date);
                            if end_date() < date {
                                end_date.set(date);
                            }
                            error.set(None);
                        },
                    }
                    if !all_day() {
                        super::date_picker::TimePicker {
                            hour: start_time().0,
                            minute: start_time().1,
                            label: crate::i18n::text("Starts", &[]),
                            on_change: move |time: (u32, u32)| { start_time.set(time); error.set(None); },
                        }
                    }
                }
            }
            div { class: "flex flex-col gap-1",
                span { class: "text-[11px] text-[var(--text-muted)]", {crate::i18n::text("Ends", &[])} }
                div { class: if all_day() { "grid grid-cols-1" } else { "grid grid-cols-[1fr_6.5rem] gap-2 items-start" },
                    super::date_picker::DatePicker {
                        value: end_date(),
                        min: Some(start_date()),
                        label: crate::i18n::text("Ends", &[]),
                        on_change: move |date: NaiveDate| { end_date.set(date); error.set(None); },
                    }
                    if !all_day() {
                        super::date_picker::TimePicker {
                            hour: end_time().0,
                            minute: end_time().1,
                            label: crate::i18n::text("Ends", &[]),
                            on_change: move |time: (u32, u32)| { end_time.set(time); error.set(None); },
                        }
                    }
                }
            }
            label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                span { class: "flex-1", {crate::i18n::text("Repeats", &[])} }
                select {
                    class: "bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-md px-2 py-1 text-xs text-[var(--text)]",
                    onchange: move |e| {
                        let picked = e.value().parse::<usize>().ok().and_then(|i| REPEATS.get(i)).map_or(EventRepeat::Never, |(r, _)| *r);
                        repeat.set(picked);
                    },
                    for (index, (value, label)) in REPEATS.iter().enumerate() {
                        option { value: "{index}", selected: repeat() == *value, {crate::i18n::label(label)} }
                    }
                }
            }
            label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                span { class: "flex-1", {crate::i18n::text("Where", &[])} }
                select {
                    class: "max-w-[60%] bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-md px-2 py-1 text-xs text-[var(--text)]",
                    onchange: move |e| host.set(e.value().parse().ok()),
                    option { value: "", selected: host().is_none(), {crate::i18n::text("No channel", &[])} }
                    for (id, label) in hosts {
                        option { value: "{id}", selected: host() == Some(id), "{label}" }
                    }
                }
            }
            div { class: "flex items-center gap-2",
                if let Some(src) = cover_src {
                    img { class: "w-16 h-10 shrink-0 rounded object-cover", src: "{src}", alt: "" }
                }
                button { r#type: "button", class: SECONDARY, onclick: pick_cover, {crate::i18n::text("Cover image…", &[])} }
                if cover().is_some() {
                    button { r#type: "button", class: SECONDARY, onclick: move |_| cover.set(None), {crate::i18n::text("Remove", &[])} }
                }
            }
            if let Some(message) = error() {
                div { class: "text-[11px] text-[var(--danger)]", role: "alert", {crate::i18n::message(&message)} }
            }
        }
        div { class: "px-4 py-3 border-t border-[var(--border)] flex items-center justify-end gap-2",
            button {
                r#type: "button",
                class: "px-3 py-1 rounded text-[11px] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                onclick: move |_| on_done.call(()),
                {crate::i18n::text("Cancel", &[])}
            }
            button {
                r#type: "button",
                class: "px-3 py-1 rounded border border-[var(--accent)] text-[11px] text-[var(--accent)] hover:bg-[var(--accent-soft)] transition-colors",
                onclick: save,
                {crate::i18n::text("Save event", &[])}
            }
        }
    }
}

/// A start notice: the server posts it, each client says it in its own language.
#[component]
pub fn EventNoticeCard(name: String, channel_id: Option<Id>) -> Element {
    let state = use_app_state();
    let host = host_label(&state.read(), channel_id);
    rsx! {
        div { class: "dxf-event-notice mt-1 w-full max-w-[28rem] flex items-center gap-3 rounded-lg border border-[var(--accent)] bg-[var(--accent-soft)] px-3 py-2",
            span { class: "block w-5 h-5 shrink-0 text-[var(--accent)]", dangerous_inner_html: super::icons::CALENDAR }
            div { class: "min-w-0 flex flex-col",
                span { class: "text-[11px] uppercase tracking-wider text-[var(--accent)]", {crate::i18n::text("Event starting now", &[])} }
                span { class: "text-sm font-semibold text-[var(--text)] break-words", "{name}" }
                if let Some(host) = host {
                    span { class: "text-[11px] text-[var(--text-muted)]", "{host}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_event_name_is_cut_to_fit_beside_the_community_name() {
        assert_eq!(shortened("Raid", 24), "Raid");
        let long = "The very long weekly community game night";
        let short = shortened(long, 24);
        assert_eq!(short.chars().count(), 24);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn a_picked_local_time_round_trips_through_utc() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let at = local_instant(day, (20, 30)).unwrap();
        assert_eq!(local_parts(at), (day, (20, 30)));
        assert!(local_instant(day, (24, 0)).is_none());
    }

    #[test]
    fn an_all_day_span_covers_its_days_from_local_midnight() {
        let first = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let (start, end) = day_span(first, first).unwrap();
        assert_eq!(local_parts(start), (first, (0, 0)));
        assert_eq!(local_parts(end), (first.succ_opt().unwrap(), (0, 0)));
        assert_eq!(
            last_day(end),
            first,
            "the end is the next midnight, the last day is the first"
        );
        let last = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        assert_eq!(last_day(day_span(first, last).unwrap().1), last);
    }
}
