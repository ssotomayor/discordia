//! Server Settings: what only the operator changes, for the whole server.
//! A field the launch configuration set is shown and cannot be edited here.

use dioxus::prelude::*;

use crate::protocol::rendezvous::GeoPoint;
use crate::protocol::{
    ClientMessage, GuildCreation, GuildVisibility, LaunchSource, ServerLimits, ServerSettings,
    ServerSettingsView,
};
use crate::state::{ConnectionStatus, use_app_state, use_gateway};

const INPUT: &str = "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors disabled:opacity-60";
const LABEL: &str =
    "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1";
const HINT: &str = "text-[10px] text-[var(--text-dim)] mt-1";
const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Identity,
    Storage,
    Guilds,
    Apps,
}

/// Numbers are kept as typed so a half-written one does not jump.
#[derive(Clone, PartialEq)]
struct Form {
    name: String,
    description: String,
    listed_public: bool,
    lat: String,
    lon: String,
    media_mb: String,
    upload_kb: String,
    avatar_kb: String,
    sticker_kb: String,
    file_days: String,
    message_days: String,
    max_guilds: String,
    operators_only: bool,
}

impl Form {
    fn from(s: &ServerSettings) -> Self {
        let l = &s.limits;
        Self {
            name: s.name.clone().unwrap_or_default(),
            description: s.description.clone().unwrap_or_default(),
            listed_public: s.listed_public,
            lat: s.location.map(|p| p.lat.to_string()).unwrap_or_default(),
            lon: s.location.map(|p| p.lon.to_string()).unwrap_or_default(),
            media_mb: (s.media_max_bytes / MIB).to_string(),
            upload_kb: (l.max_upload_bytes / 1000).to_string(),
            avatar_kb: (l.max_avatar_bytes / 1000).to_string(),
            sticker_kb: (l.max_sticker_bytes / 1000).to_string(),
            file_days: l.max_file_retention_days.to_string(),
            message_days: l
                .max_message_retention_days
                .map(|d| d.to_string())
                .unwrap_or_default(),
            max_guilds: l.max_guilds_per_owner.to_string(),
            operators_only: l.guild_creation == GuildCreation::Operators,
        }
    }

    /// The server validates again; this only catches what is not a number.
    fn parse(&self) -> Result<ServerSettings, String> {
        let number = |text: &str, what: &str| {
            text.trim()
                .parse::<u64>()
                .map_err(|_| crate::i18n::text("{0} must be a whole number.", &[what.to_string()]))
        };
        let blank = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
        let location = match (blank(&self.lat), blank(&self.lon)) {
            (None, None) => None,
            (Some(lat), Some(lon)) => Some(GeoPoint {
                lat: lat.parse().map_err(|_| bad_location())?,
                lon: lon.parse().map_err(|_| bad_location())?,
            }),
            _ => return Err(bad_location()),
        };
        let message_days = match blank(&self.message_days) {
            Some(days) => Some(
                u32::try_from(number(&days, &crate::i18n::text("Message history", &[]))?)
                    .map_err(|_| crate::i18n::text("Too many days.", &[]))?,
            ),
            None => None,
        };
        let small = |text: &str, what: &str| {
            number(text, what).and_then(|n| {
                u32::try_from(n)
                    .map_err(|_| crate::i18n::text("{0} is too large.", &[what.to_string()]))
            })
        };
        Ok(ServerSettings {
            name: blank(&self.name),
            description: blank(&self.description),
            listed_public: self.listed_public,
            location,
            media_max_bytes: number(&self.media_mb, &crate::i18n::text("Media storage", &[]))?
                .saturating_mul(MIB),
            limits: ServerLimits {
                max_upload_bytes: number(&self.upload_kb, &crate::i18n::text("Chat files", &[]))?
                    .saturating_mul(1000),
                max_avatar_bytes: number(
                    &self.avatar_kb,
                    &crate::i18n::text("Avatars and banners", &[]),
                )?
                .saturating_mul(1000),
                max_sticker_bytes: number(
                    &self.sticker_kb,
                    &crate::i18n::text("Emojis and stickers", &[]),
                )?
                .saturating_mul(1000),
                max_file_retention_days: small(
                    &self.file_days,
                    &crate::i18n::text("File expiry", &[]),
                )?,
                max_message_retention_days: message_days,
                max_guilds_per_owner: small(
                    &self.max_guilds,
                    &crate::i18n::text("Guilds per person", &[]),
                )?,
                guild_creation: if self.operators_only {
                    GuildCreation::Operators
                } else {
                    GuildCreation::Anyone
                },
            },
        })
    }
}

fn bad_location() -> String {
    crate::i18n::text(
        "A location is a latitude and a longitude, or both left blank.",
        &[],
    )
}

/// History a lower ceiling deletes for good, so it asks first.
fn shortens_history(before: Option<u32>, after: Option<u32>) -> Option<u32> {
    match (before, after) {
        (_, None) => None,
        (None, Some(days)) => Some(days),
        (Some(old), Some(days)) => (days < old).then_some(days),
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / MIB as f64)
}

#[component]
pub fn ServerSettingsButton() -> Element {
    let mut state = use_app_state();
    let shown = {
        let s = state.read();
        s.is_operator && s.status == ConnectionStatus::Ready
    };
    if !shown {
        return rsx! {};
    }
    rsx! {
        button {
            class: "dxf-workspace-server-settings w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
            title: crate::i18n::text("Server Settings", &[]),
            aria_label: crate::i18n::text("Server Settings", &[]),
            onclick: move |_| state.write().server_settings_open = true,
            span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::SERVER }
        }
    }
}

#[component]
pub fn ServerSettingsDialog() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let open = use_memo(move || {
        let s = state.read();
        s.server_settings_open && s.is_operator && s.status == ConnectionStatus::Ready
    });
    {
        let gateway = gateway.clone();
        use_effect(move || {
            if open() {
                gateway.send(ClientMessage::FetchServerSettings);
            }
        });
    }
    let mut section = use_signal(|| Section::Identity);
    if !open() {
        return rsx! {};
    }
    let close = move |_| state.write().server_settings_open = false;
    let view = state.read().server_settings.clone();
    let item = move |s: Section| {
        if section() == s {
            "w-full text-left rounded px-2 py-1.5 text-xs text-[var(--text)] bg-[var(--accent-soft)]"
        } else {
            "w-full text-left rounded px-2 py-1.5 text-xs text-[var(--text)] hover:bg-[var(--hover)]"
        }
    };
    let sections = [
        (Section::Identity, crate::i18n::text("Identity", &[])),
        (
            Section::Storage,
            crate::i18n::text("Storage and limits", &[]),
        ),
        (Section::Guilds, crate::i18n::text("Guilds", &[])),
        (Section::Apps, crate::i18n::text("Apps", &[])),
    ];
    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in w-[44rem] max-w-[95vw] h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                role: "dialog",
                aria_modal: "true",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", {crate::i18n::text("Server Settings", &[])} }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        aria_label: crate::i18n::text("Close", &[]),
                        onclick: close,
                        "✕"
                    }
                }
                div { class: "flex-1 min-h-0 flex",
                    nav { class: "w-44 shrink-0 border-r border-[var(--border)] p-2 space-y-1",
                        for (s, label) in sections {
                            button { key: "{label}", class: item(s), onclick: move |_| section.set(s), "{label}" }
                        }
                    }
                    div { class: "flex-1 min-w-0 overflow-y-auto p-4",
                        match (section(), view) {
                            (Section::Apps, _) => rsx! { crate::features::agents::ServerAppsPanel {} },
                            (_, None) => rsx! {
                                div { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Loading…", &[])} }
                            },
                            (current, Some(view)) => {
                                let version = format!("{:?}", view.settings);
                                rsx! {
                                    SettingsForm { key: "{version}", section: current, view }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Keyed by the saved settings: a save, or another operator's, arrives as a
/// new view and the form starts again from it.
#[component]
fn SettingsForm(section: Section, view: ServerSettingsView) -> Element {
    let gateway = use_gateway();
    let mut form = use_signal({
        let saved = view.settings.clone();
        move || Form::from(&saved)
    });
    let mut error = use_signal(|| None::<String>);
    let mut confirm = use_signal(|| None::<u32>);
    let dirty = form() != Form::from(&view.settings);
    let pinned = view.pinned.clone();
    let pin_hint = match view.source {
        LaunchSource::Config => crate::i18n::text(
            "Set in the server's configuration, which takes precedence.",
            &[],
        ),
        LaunchSource::SelfHost => crate::i18n::text(
            "Set in the form you host from; change it there next time you host.",
            &[],
        ),
    };
    let submit = {
        let gateway = gateway.clone();
        let current = view.settings.limits.max_message_retention_days;
        move |confirmed: bool| match form.peek().parse() {
            Ok(next) => {
                let shortens = shortens_history(current, next.limits.max_message_retention_days);
                if let (Some(days), false) = (shortens, confirmed) {
                    confirm.set(Some(days));
                    return;
                }
                error.set(None);
                confirm.set(None);
                gateway.send(ClientMessage::SetServerSettings { settings: next });
            }
            Err(message) => error.set(Some(message)),
        }
    };
    let f = form();
    let text_field = move |value: String, disabled: bool, set: fn(&mut Form, String)| -> Element {
        rsx! {
            input {
                class: INPUT,
                value: "{value}",
                disabled,
                oninput: move |e| set(&mut form.write(), e.value()),
            }
        }
    };
    let body = match section {
        Section::Identity => rsx! {
            if !view.publishes {
                div { class: "{HINT} mb-3",
                    {crate::i18n::text("This server is not published on a rendezvous, so the name only labels it in members' server list.", &[])}
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Server name", &[])} }
                {text_field(f.name.clone(), pinned.name, |f, v| f.name = v)}
                div { class: HINT,
                    if pinned.name { "{pin_hint}" } else { {crate::i18n::text("What friends type to find it, and what the server list calls it. Blank asks the rendezvous for a random code.", &[])} }
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Description", &[])} }
                {text_field(f.description.clone(), pinned.description, |f, v| f.description = v)}
                div { class: HINT,
                    if pinned.description { "{pin_hint}" } else { {crate::i18n::text("One line shown beside the name.", &[])} }
                }
            }
            div {
                label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                    input {
                        r#type: "checkbox",
                        checked: f.listed_public,
                        disabled: pinned.listed_public,
                        onchange: move |e| form.write().listed_public = e.checked(),
                    }
                    {crate::i18n::text("List it in the public directory", &[])}
                }
                div { class: HINT,
                    if pinned.listed_public { "{pin_hint}" } else { {crate::i18n::text("Otherwise only someone who knows the name or code finds it.", &[])} }
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Location on the globe", &[])} }
                div { class: "flex gap-2",
                    input {
                        class: INPUT,
                        placeholder: crate::i18n::text("Latitude", &[]),
                        value: "{f.lat}",
                        disabled: pinned.location,
                        oninput: move |e| form.write().lat = e.value(),
                    }
                    input {
                        class: INPUT,
                        placeholder: crate::i18n::text("Longitude", &[]),
                        value: "{f.lon}",
                        disabled: pinned.location,
                        oninput: move |e| form.write().lon = e.value(),
                    }
                }
                div { class: HINT,
                    if pinned.location { "{pin_hint}" } else { {crate::i18n::text("Self-declared and kept to a tenth of a degree. Leave both blank to stay off the globe.", &[])} }
                }
            }
        },
        Section::Storage => rsx! {
            div {
                div { class: LABEL, {crate::i18n::text("Media storage (MB)", &[])} }
                input {
                    class: INPUT,
                    r#type: "number",
                    min: (crate::protocol::MIN_MEDIA_MAX_BYTES / MIB).to_string(),
                    value: "{f.media_mb}",
                    disabled: pinned.media_max_bytes,
                    oninput: move |e| form.write().media_mb = e.value(),
                }
                div { class: HINT,
                    {crate::i18n::text("{0} MB in use. Past the cap, uploads are refused; nothing is deleted.", &[megabytes(view.media_used_bytes)])}
                    if pinned.media_max_bytes { " {pin_hint}" }
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Largest upload any guild may allow (KB)", &[])} }
                div { class: "grid grid-cols-3 gap-2",
                    label { class: "flex flex-col gap-1 text-[10px] text-[var(--text-dim)]",
                        {crate::i18n::text("Chat files", &[])}
                        input { class: INPUT, r#type: "number", min: 1, max: 10000, value: "{f.upload_kb}", oninput: move |e| form.write().upload_kb = e.value() }
                    }
                    label { class: "flex flex-col gap-1 text-[10px] text-[var(--text-dim)]",
                        {crate::i18n::text("Avatars and banners", &[])}
                        input { class: INPUT, r#type: "number", min: 1, max: 10000, value: "{f.avatar_kb}", oninput: move |e| form.write().avatar_kb = e.value() }
                    }
                    label { class: "flex flex-col gap-1 text-[10px] text-[var(--text-dim)]",
                        {crate::i18n::text("Emojis and stickers", &[])}
                        input { class: INPUT, r#type: "number", min: 1, max: 10000, value: "{f.sticker_kb}", oninput: move |e| form.write().sticker_kb = e.value() }
                    }
                }
                div { class: HINT, {crate::i18n::text("Up to 10000 KB. A guild keeps its own lower limit; one set higher is held to this.", &[])} }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Longest a chat file is kept (days)", &[])} }
                input { class: INPUT, r#type: "number", min: 1, max: 365, value: "{f.file_days}", oninput: move |e| form.write().file_days = e.value() }
                div { class: HINT, {crate::i18n::text("Applies to files uploaded from now on.", &[])} }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Longest message history any guild keeps (days)", &[])} }
                input {
                    class: INPUT,
                    r#type: "number",
                    min: 1,
                    max: 3650,
                    placeholder: crate::i18n::text("forever", &[]),
                    value: "{f.message_days}",
                    oninput: move |e| form.write().message_days = e.value(),
                }
                div { class: HINT, {crate::i18n::text("Blank lets each guild decide. Older messages are deleted by the hourly sweep, in every guild.", &[])} }
            }
        },
        _ => rsx! {
            div {
                div { class: LABEL, {crate::i18n::text("Who may create guilds", &[])} }
                label { class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)]",
                    input { r#type: "radio", name: "dxf-guild-creation", checked: !f.operators_only, onchange: move |_| form.write().operators_only = false }
                    {crate::i18n::text("Anyone on this server", &[])}
                }
                label { class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)]",
                    input { r#type: "radio", name: "dxf-guild-creation", checked: f.operators_only, onchange: move |_| form.write().operators_only = true }
                    {crate::i18n::text("Only operators", &[])}
                }
                div { class: HINT, {crate::i18n::text("Guilds that already exist stay.", &[])} }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Guilds one person may own", &[])} }
                input { class: INPUT, r#type: "number", min: 1, max: 1000, value: "{f.max_guilds}", oninput: move |e| form.write().max_guilds = e.value() }
            }
            DefaultGuildPicker {}
        },
    };
    rsx! {
        form {
            class: "space-y-4",
            onsubmit: {
                let mut submit = submit.clone();
                move |e: FormEvent| {
                    e.prevent_default();
                    submit(false);
                }
            },
            {body}
            if let Some(days) = confirm() {
                div { class: "rounded border border-[var(--danger)] p-2 space-y-2",
                    div { class: "text-xs text-[var(--text)]",
                        {crate::i18n::text("Messages older than {0} days will be deleted in every guild within the hour. This cannot be undone.", &[days.to_string()])}
                    }
                    div { class: "flex gap-2 justify-end",
                        button {
                            r#type: "button",
                            class: "rounded px-2.5 py-1 text-[11px] text-[var(--text-dim)] border border-[var(--border)]",
                            onclick: move |_| confirm.set(None),
                            {crate::i18n::text("Cancel", &[])}
                        }
                        button {
                            r#type: "button",
                            class: "rounded px-2.5 py-1 text-[11px] text-[var(--danger)] border border-[var(--danger)]",
                            onclick: {
                                let mut submit = submit.clone();
                                move |_| submit(true)
                            },
                            {crate::i18n::text("Delete and save", &[])}
                        }
                    }
                }
            }
            if section != Section::Guilds || dirty {
                div { class: "flex items-center gap-2 pt-1",
                    if let Some(message) = error() {
                        span { class: "text-[var(--danger)] text-xs", {crate::i18n::message(&message)} }
                    }
                    button {
                        r#type: "submit",
                        class: "dxf-cta ml-auto rounded-lg px-3 py-1 text-xs",
                        disabled: !dirty || confirm().is_some(),
                        {crate::i18n::text("Save", &[])}
                    }
                }
            }
        }
    }
}

/// Applied at once, like the guild menu's own toggle: it is its own command.
#[component]
fn DefaultGuildPicker() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let (current, public, unlisted) = {
        let s = state.read();
        let public: Vec<(crate::protocol::Id, String)> = s
            .guilds
            .iter()
            .filter(|g| g.visibility == GuildVisibility::Public)
            .map(|g| (g.id, g.name.clone()))
            .collect();
        let unlisted = unlisted_default(&s.guilds, s.default_guild);
        (s.default_guild, public, unlisted)
    };
    let selected = current.map(|id| id.to_string()).unwrap_or_default();
    rsx! {
        div {
            div { class: LABEL, {crate::i18n::text("Where newcomers land", &[])} }
            select {
                class: INPUT,
                value: "{selected}",
                onchange: move |e| {
                    let guild_id = e.value().parse().ok();
                    gateway.send(ClientMessage::SetDefaultGuild { guild_id });
                },
                option { value: "", selected: current.is_none(), {crate::i18n::text("The system guilds", &[])} }
                for (id, name) in public {
                    option { key: "{id}", value: "{id}", selected: current == Some(id), "{name}" }
                }
                if let (Some(id), Some(unlisted)) = (current, unlisted.clone()) {
                    option { key: "{id}", value: "{id}", selected: true, {unlisted.label()} }
                }
            }
            div { class: HINT, {crate::i18n::text("Only public guilds you belong to are listed. Applies at once.", &[])} }
            if matches!(unlisted, Some(UnlistedDefault::Private(_))) {
                div { class: "text-[10px] text-[var(--warn)] mt-1",
                    {crate::i18n::text("This guild is private, so newcomers join no guild. They can still create one or browse public guilds.", &[])}
                }
            }
        }
    }
}

/// The default when it is not one of the public guilds listed: a select whose value
/// matches no option shows its first, "The system guilds", which is where nobody lands.
#[derive(Clone, Debug, PartialEq)]
enum UnlistedDefault {
    Private(String),
    NotJoined,
}

impl UnlistedDefault {
    fn label(&self) -> String {
        match self {
            Self::Private(name) => crate::i18n::text(
                "{0} (private: newcomers land nowhere)",
                std::slice::from_ref(name),
            ),
            Self::NotJoined => crate::i18n::text("A guild you're not in", &[]),
        }
    }
}

fn unlisted_default(
    guilds: &[crate::protocol::Guild],
    current: Option<crate::protocol::Id>,
) -> Option<UnlistedDefault> {
    let id = current?;
    match guilds.iter().find(|g| g.id == id) {
        Some(g) if g.visibility == GuildVisibility::Public => None,
        Some(g) => Some(UnlistedDefault::Private(g.name.clone())),
        None => Some(UnlistedDefault::NotJoined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_the_picker_does_not_list_is_still_shown() {
        let guild = |visibility| {
            let mut guild: crate::protocol::Guild = serde_json::from_value(serde_json::json!({
                "id": crate::protocol::Id::new_v4(), "name": "Lobby", "icon": null, "owner_pubkey": ""
            }))
            .unwrap();
            guild.visibility = visibility;
            guild
        };
        let public = guild(GuildVisibility::Public);
        let private = guild(GuildVisibility::Private);
        let listed = std::slice::from_ref(&public);
        assert_eq!(unlisted_default(listed, None), None);
        assert_eq!(unlisted_default(listed, Some(public.id)), None);
        assert_eq!(
            unlisted_default(std::slice::from_ref(&private), Some(private.id)),
            Some(UnlistedDefault::Private("Lobby".into()))
        );
        assert_eq!(
            unlisted_default(&[], Some(private.id)),
            Some(UnlistedDefault::NotJoined)
        );
    }

    #[test]
    fn only_a_shorter_history_asks_first() {
        assert_eq!(shortens_history(None, Some(30)), Some(30));
        assert_eq!(shortens_history(Some(30), Some(10)), Some(10));
        assert_eq!(shortens_history(Some(10), Some(30)), None);
        assert_eq!(shortens_history(Some(10), None), None);
    }

    #[test]
    fn a_form_reads_back_what_it_was_made_from() {
        let settings = ServerSettings {
            name: Some("Ana's".into()),
            location: Some(GeoPoint {
                lat: -34.6,
                lon: -58.4,
            }),
            media_max_bytes: 512 * MIB,
            ..ServerSettings::default()
        };
        assert_eq!(Form::from(&settings).parse().unwrap(), settings);
    }

    #[test]
    fn half_a_location_is_refused() {
        let mut form = Form::from(&ServerSettings::default());
        form.lat = "10".into();
        assert!(form.parse().is_err());
    }
}
