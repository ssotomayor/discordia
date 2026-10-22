use std::collections::HashMap;

use dioxus::prelude::*;

use crate::protocol::{
    ArgValue, BotCommand, ChannelKind, ClientMessage, CommandArg, CommandOption, Id, OptionKind,
    option_shown,
};
use crate::state::{CommandForm, use_app_state, use_gateway};

const HEADING: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";
const FIELD: &str = "w-full bg-[var(--panel-solid)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors";
/// Enough to find someone by typing; the rest are a narrower search away.
const PICKER_ROWS: usize = 50;

fn arg_of(
    opt: &CommandOption,
    values: &HashMap<String, String>,
    picked: &HashMap<String, Vec<String>>,
) -> Result<Option<ArgValue>, String> {
    let raw = values.get(&opt.id).map(|v| v.trim()).unwrap_or("");
    Ok(match &opt.kind {
        OptionKind::Text { .. } if !raw.is_empty() => Some(ArgValue::Text(raw.to_string())),
        OptionKind::Integer { .. } if !raw.is_empty() => {
            Some(ArgValue::Integer(raw.parse().map_err(|_| {
                format!("{} must be a whole number", opt.label)
            })?))
        }
        OptionKind::Choice { .. } if !raw.is_empty() => Some(ArgValue::Choice(raw.to_string())),
        OptionKind::Members { .. } => picked
            .get(&opt.id)
            .filter(|keys| !keys.is_empty())
            .map(|keys| ArgValue::Members(keys.clone())),
        _ => None,
    })
}

/// What the form holds, turned into what the server accepts. Runs the same
/// check the server does, so a mistake is named here instead of in a toast.
/// A field the current choices hide is left out even if it was filled in.
fn build_args(
    cmd: &BotCommand,
    values: &HashMap<String, String>,
    picked: &HashMap<String, Vec<String>>,
) -> Result<Vec<CommandArg>, String> {
    let mut args = Vec::new();
    for opt in &cmd.options {
        if !option_shown(opt, &args) {
            continue;
        }
        if let Some(value) = arg_of(opt, values, picked)? {
            args.push(CommandArg {
                id: opt.id.clone(),
                value,
            });
        }
    }
    crate::protocol::check_args(cmd, args)
}

/// The fields to draw, in order, for the choices made so far.
fn shown_options<'a>(
    cmd: &'a BotCommand,
    values: &HashMap<String, String>,
) -> Vec<&'a CommandOption> {
    let mut choices = Vec::new();
    let mut shown = Vec::new();
    for opt in &cmd.options {
        if !option_shown(opt, &choices) {
            continue;
        }
        if let (OptionKind::Choice { .. }, Some(v)) = (&opt.kind, values.get(&opt.id))
            && !v.is_empty()
        {
            choices.push(CommandArg {
                id: opt.id.clone(),
                value: ArgValue::Choice(v.clone()),
            });
        }
        shown.push(opt);
    }
    shown
}

fn first_choices(cmd: &BotCommand) -> HashMap<String, String> {
    cmd.options
        .iter()
        .filter(|o| o.required)
        .filter_map(|o| match &o.kind {
            OptionKind::Choice { choices } => {
                choices.first().map(|c| (o.id.clone(), c.value.clone()))
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, PartialEq)]
struct Person {
    pubkey: String,
    name: String,
    npub: String,
}

/// A name anywhere in it, or the start of a key in either spelling.
fn person_matches(p: &Person, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || p.name.to_lowercase().contains(&needle)
        || p.pubkey.starts_with(&needle)
        || p.npub.starts_with(&needle)
}

/// The buttons on a bot's profile card. Hidden where the viewer lacks a
/// command's permission; the server refuses it regardless (trap 5). One with
/// fields opens `CommandDialog` instead of running.
#[component]
pub fn BotCommandsSection(bot_pubkey: String) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let s = state.read();
    let Some(guild_id) = s.selected_guild else {
        return rsx! { Fragment {} };
    };
    let Some(online) = s
        .members
        .iter()
        .find(|m| m.guild_id == guild_id && m.user.pubkey == bot_pubkey && m.bot)
        .map(|m| m.online)
    else {
        return rsx! { Fragment {} };
    };
    let commands: Vec<BotCommand> = s
        .bot_commands
        .get(&bot_pubkey)
        .map(|list| {
            list.iter()
                .filter(|c| c.permission.is_none_or(|p| s.can(guild_id, p)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if commands.is_empty() {
        return rsx! { Fragment {} };
    }
    let channel = selected_text_channel(&s, guild_id);
    drop(s);
    let usable = online && channel.is_some();

    rsx! {
        div { class: "mt-3",
            div { class: "flex items-center justify-between mb-1.5",
                span { class: HEADING, {crate::i18n::text("Commands", &[])} }
                {where_it_runs(online, channel.as_ref().map(|(_, name)| name.as_str()))}
            }
            div { class: "flex flex-wrap gap-1.5",
                for cmd in commands {
                    {
                        let channel_id = channel.as_ref().map(|(id, _)| *id);
                        let gateway = gateway.clone();
                        let bot_pubkey = bot_pubkey.clone();
                        rsx! {
                            button {
                                key: "{cmd.id}",
                                class: "px-2.5 py-1 rounded-lg text-xs border border-[var(--border)] text-[var(--text)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors disabled:opacity-40",
                                disabled: !usable,
                                title: "{cmd.description}",
                                onclick: move |_| {
                                    let Some(channel_id) = channel_id else { return };
                                    let mut s = state.write();
                                    if cmd.options.is_empty() {
                                        gateway.send(ClientMessage::InvokeCommand {
                                            guild_id,
                                            channel_id,
                                            bot_pubkey: bot_pubkey.clone(),
                                            command: cmd.id.clone(),
                                            args: Vec::new(),
                                            parent: None,
                                        });
                                    } else {
                                        s.command_form = Some(CommandForm {
                                            bot_pubkey: bot_pubkey.clone(),
                                            command: cmd.id.clone(),
                                        });
                                    }
                                    s.profile_card = None;
                                },
                                if cmd.options.is_empty() { "{cmd.label}" } else { "{cmd.label} …" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn selected_text_channel(s: &crate::state::AppState, guild_id: Id) -> Option<(Id, String)> {
    s.selected_channel
        .and_then(|id| {
            s.channels
                .iter()
                .find(|c| c.id == id && c.guild_id == guild_id && c.kind == ChannelKind::Text)
        })
        .map(|c| (c.id, c.name.clone()))
}

fn where_it_runs(online: bool, channel: Option<&str>) -> Element {
    let text = match (channel, online) {
        (_, false) => crate::i18n::text("offline", &[]),
        (None, true) => crate::i18n::text("open a text channel to use them", &[]),
        (Some(name), true) => crate::i18n::text("runs in #{0}", &[name.to_string()]),
    };
    rsx! { span { class: "text-[10px] text-[var(--text-dim)]", "{text}" } }
}

/// The open command form, if any. Mounted at the workspace root (trap 22);
/// keyed in a one-element list so a second form starts empty (trap 6).
#[component]
pub fn CommandDialogHost() -> Element {
    let state = use_app_state();
    let form = state.read().command_form.clone();
    rsx! {
        for f in form.into_iter() {
            CommandDialog { key: "{f.bot_pubkey}/{f.command}", form: f }
        }
    }
}

#[component]
fn CommandDialog(form: CommandForm) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let s = state.read();
    let guild_id = s.selected_guild;
    let cmd = guild_id.and_then(|g| {
        s.bot_commands
            .get(&form.bot_pubkey)
            .and_then(|list| list.iter().find(|c| c.id == form.command).cloned())
            .filter(|c| c.permission.is_none_or(|p| s.can(g, p)))
    });
    let online = guild_id.is_some_and(|g| {
        s.members
            .iter()
            .any(|m| m.guild_id == g && m.user.pubkey == form.bot_pubkey && m.bot && m.online)
    });
    let channel = guild_id.and_then(|g| selected_text_channel(&s, g));
    let bot_name = s.display_name(&form.bot_pubkey);
    let people: Vec<Person> = guild_id
        .map(|g| {
            s.members_of(g)
                .into_iter()
                .filter(|m| !m.bot)
                .map(|m| Person {
                    pubkey: m.user.pubkey.clone(),
                    name: s.display_name(&m.user.pubkey),
                    npub: crate::identity::npub_of(&m.user.pubkey),
                })
                .collect()
        })
        .unwrap_or_default();
    drop(s);

    let initial = cmd.as_ref().map(first_choices).unwrap_or_default();
    let values = use_signal(move || initial);
    let picked = use_signal(HashMap::<String, Vec<String>>::new);
    let searches = use_signal(HashMap::<String, String>::new);
    let mut picker_open = use_signal(|| None::<String>);
    let mut form_error = use_signal(|| None::<String>);

    let close = move |_| state.write().command_form = None;
    let (Some(cmd), Some(guild_id)) = (cmd, guild_id) else {
        return rsx! { Fragment {} };
    };
    let shown: Vec<CommandOption> = shown_options(&cmd, &values.read())
        .into_iter()
        .cloned()
        .collect();
    let run_cmd = cmd.clone();
    let runnable = online && channel.is_some();

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in w-[26rem] max-w-full max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl",
                onclick: move |e| {
                    e.stop_propagation();
                    picker_open.set(None);
                },
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center gap-2",
                    div { class: "flex-1 min-w-0",
                        h3 { class: "text-sm font-medium text-[var(--accent)] truncate", "{cmd.label}" }
                        div { class: "text-[11px] text-[var(--text-dim)] truncate", "{bot_name}" }
                    }
                    {where_it_runs(online, channel.as_ref().map(|(_, name)| name.as_str()))}
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: close,
                        "×"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-4 flex flex-col gap-3",
                    if !cmd.description.is_empty() {
                        div { class: "text-xs text-[var(--text-muted)]", "{cmd.description}" }
                    }
                    for opt in shown {
                        div { key: "{opt.id}", class: "flex flex-col gap-1",
                            span { class: HEADING,
                                if opt.required { "{opt.label}" } else { "{opt.label} " span { class: "normal-case font-normal", {crate::i18n::text("(optional)", &[])} } }
                            }
                            if !opt.description.is_empty() {
                                span { class: "text-[11px] text-[var(--text-dim)]", "{opt.description}" }
                            }
                            {field(&opt, values, picked, searches, picker_open, &people)}
                        }
                    }
                    if let Some(err) = form_error() {
                        div { class: "text-xs text-[var(--danger)]", {crate::i18n::message(&err)} }
                    }
                }
                div { class: "px-4 py-3 border-t border-[var(--border)] flex gap-2 justify-end",
                    button {
                        class: "px-3 py-1 rounded-lg text-xs text-[var(--text-muted)] hover:text-[var(--text)]",
                        onclick: close,
                        {crate::i18n::text("Cancel", &[])}
                    }
                    button {
                        class: "dxf-cta px-3 py-1 rounded-lg text-xs disabled:opacity-40",
                        disabled: !runnable,
                        onclick: move |_| {
                            let Some((channel_id, _)) = channel.clone() else { return };
                            match build_args(&run_cmd, &values.read(), &picked.read()) {
                                Ok(args) => {
                                    gateway.send(ClientMessage::InvokeCommand {
                                        guild_id,
                                        channel_id,
                                        bot_pubkey: form.bot_pubkey.clone(),
                                        command: run_cmd.id.clone(),
                                        args,
                                        parent: None,
                                    });
                                    state.write().command_form = None;
                                }
                                Err(e) => form_error.set(Some(e)),
                            }
                        },
                        {crate::i18n::text("Run", &[])}
                    }
                }
            }
        }
    }
}

fn field(
    opt: &CommandOption,
    mut values: Signal<HashMap<String, String>>,
    picked: Signal<HashMap<String, Vec<String>>>,
    searches: Signal<HashMap<String, String>>,
    picker_open: Signal<Option<String>>,
    people: &[Person],
) -> Element {
    let id = opt.id.clone();
    let current = values.read().get(&id).cloned().unwrap_or_default();
    match opt.kind.clone() {
        OptionKind::Text { max_len } => rsx! {
            input {
                class: FIELD,
                maxlength: max_len as i64,
                value: "{current}",
                oninput: move |e| { values.write().insert(id.clone(), e.value()); },
            }
        },
        OptionKind::Integer { min, max } => rsx! {
            input {
                class: FIELD,
                r#type: "number",
                min: min,
                max: max,
                placeholder: "{min} – {max}",
                value: "{current}",
                oninput: move |e| { values.write().insert(id.clone(), e.value()); },
            }
        },
        OptionKind::Choice { choices } => rsx! {
            select {
                class: FIELD,
                value: "{current}",
                onchange: move |e| { values.write().insert(id.clone(), e.value()); },
                if !opt.required {
                    option { value: "", "—" }
                }
                for c in choices.iter() {
                    option { key: "{c.value}", value: "{c.value}", selected: c.value == current, "{c.label}" }
                }
            }
        },
        OptionKind::Members { max, .. } => {
            member_picker(id, max, picked, searches, picker_open, people)
        }
    }
}

/// Several people from one search box: chips for who is picked, and a list
/// that narrows by name or key as you type.
fn member_picker(
    id: String,
    max: u8,
    mut picked: Signal<HashMap<String, Vec<String>>>,
    mut searches: Signal<HashMap<String, String>>,
    mut picker_open: Signal<Option<String>>,
    people: &[Person],
) -> Element {
    let chosen = picked.read().get(&id).cloned().unwrap_or_default();
    let needle = searches.read().get(&id).cloned().unwrap_or_default();
    let open = picker_open.read().as_deref() == Some(id.as_str());
    let full = chosen.len() >= max as usize && max > 1;
    let chips: Vec<Person> = chosen
        .iter()
        .filter_map(|k| people.iter().find(|p| &p.pubkey == k).cloned())
        .collect();
    let matches: Vec<Person> = people
        .iter()
        .filter(|p| !chosen.contains(&p.pubkey) && person_matches(p, &needle))
        .take(PICKER_ROWS)
        .cloned()
        .collect();
    let id_input = id.clone();
    let id_focus = id.clone();
    rsx! {
        div { class: "relative flex flex-col gap-1",
            onclick: move |e| e.stop_propagation(),
            if !chips.is_empty() {
                div { class: "flex flex-wrap gap-1",
                    for p in chips {
                        {
                            let id = id.clone();
                            let pk = p.pubkey.clone();
                            rsx! {
                                span {
                                    key: "{p.pubkey}",
                                    class: "inline-flex items-center gap-1 pl-2 pr-1 py-0.5 rounded-md text-xs border border-[var(--accent)] text-[var(--accent)] bg-[var(--accent-soft)]",
                                    "{p.name}"
                                    button {
                                        class: "px-1 leading-none hover:text-[var(--text)]",
                                        title: crate::i18n::text("Remove", &[]),
                                        onclick: move |_| {
                                            if let Some(list) = picked.write().get_mut(&id) {
                                                list.retain(|k| k != &pk);
                                            }
                                        },
                                        "×"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            input {
                class: FIELD,
                placeholder: crate::i18n::text("Search by name or npub", &[]),
                disabled: full,
                value: "{needle}",
                onfocus: move |_| picker_open.set(Some(id_focus.clone())),
                oninput: move |e| {
                    searches.write().insert(id_input.clone(), e.value());
                    picker_open.set(Some(id_input.clone()));
                },
                onkeydown: move |e| {
                    if e.key() == Key::Escape {
                        picker_open.set(None);
                    }
                },
            }
            if open {
                div { class: "absolute z-10 left-0 right-0 top-full mt-1 max-h-48 overflow-y-auto rounded border border-[var(--border)] bg-[var(--panel-solid)] shadow-lg py-1",
                    if matches.is_empty() {
                        div { class: "px-2 py-1.5 text-xs text-[var(--text-dim)]", {crate::i18n::text("No one matches", &[])} }
                    }
                    for p in matches {
                        {
                            let id = id.clone();
                            let pk = p.pubkey.clone();
                            let short = crate::identity::truncate_pubkey(&p.npub);
                            rsx! {
                                button {
                                    key: "{p.pubkey}",
                                    class: "w-full text-left px-2 py-1 flex items-center gap-2 text-xs text-[var(--text)] hover:bg-white/[0.04]",
                                    onclick: move |_| {
                                        let mut all = picked.write();
                                        let list = all.entry(id.clone()).or_default();
                                        if max == 1 {
                                            *list = vec![pk.clone()];
                                        } else if list.len() < max as usize {
                                            list.push(pk.clone());
                                        }
                                        let now_full = list.len() >= max as usize;
                                        drop(all);
                                        searches.write().remove(&id);
                                        if now_full {
                                            picker_open.set(None);
                                        }
                                    },
                                    span { class: "flex-1 truncate", "{p.name}" }
                                    span { class: "font-mono text-[10px] text-[var(--text-dim)]", "{short}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A bot's private answers in this channel, below the messages. Only this
/// session has them: the server never stored them.
#[component]
pub fn CommandNotes(channel_id: Id) -> Element {
    let mut state = use_app_state();
    let notes: Vec<(Id, String, String)> = {
        let s = state.read();
        s.command_notes
            .iter()
            .filter(|n| n.channel_id == channel_id)
            .map(|n| {
                (
                    n.invocation_id,
                    s.display_name(&n.bot_pubkey),
                    n.content.clone(),
                )
            })
            .collect()
    };
    rsx! {
        for (id, bot, content) in notes {
            div {
                key: "{id}",
                class: "dxf-fade mt-2 rounded-lg border border-dashed border-[var(--border)] px-3 py-2",
                div { class: "flex items-center gap-2",
                    span { class: "font-semibold text-[var(--accent)] text-xs", "{bot}" }
                    span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("only you can see this", &[])} }
                    button {
                        class: "ml-auto text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)]",
                        onclick: move |_| state.write().command_notes.retain(|n| n.invocation_id != id),
                        {crate::i18n::text("Dismiss", &[])}
                    }
                }
                div { class: "mt-1 text-sm text-[var(--text)] whitespace-pre-wrap break-words", "{content}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CommandOption;

    fn cmd() -> BotCommand {
        BotCommand::new("lechear", "Lechear")
            .option(CommandOption::choice(
                "game",
                "Juego",
                &[("smash", "Smash")],
            ))
            .option(CommandOption::integer("kills", "Kills", 0, 30).optional())
    }

    #[test]
    fn a_form_becomes_the_args_the_server_checks() {
        let mut values = first_choices(&cmd());
        values.insert("kills".into(), " 12 ".into());
        let args = build_args(&cmd(), &values, &HashMap::new()).unwrap();
        assert_eq!(args[0].value, ArgValue::Choice("smash".into()));
        assert_eq!(args[1].value, ArgValue::Integer(12));
    }

    #[test]
    fn a_bad_number_is_named_before_it_is_sent() {
        let mut values = first_choices(&cmd());
        values.insert("kills".into(), "doce".into());
        let err = build_args(&cmd(), &values, &HashMap::new()).unwrap_err();
        assert!(err.contains("Kills"), "{err}");
        values.insert("kills".into(), "99".into());
        assert!(build_args(&cmd(), &values, &HashMap::new()).is_err());
    }

    fn per_game() -> BotCommand {
        BotCommand::new("lechear", "Lechear")
            .option(CommandOption::choice(
                "game",
                "Juego",
                &[("smash", "Smash"), ("cod", "CoD")],
            ))
            .option(
                CommandOption::integer("players", "Jugadores", 1, 8).shown_when("game", &["smash"]),
            )
            .option(
                CommandOption::integer("kills", "Kills", 1, 30)
                    .optional()
                    .shown_when("game", &["cod"]),
            )
    }

    #[test]
    fn the_fields_follow_the_choice() {
        let cmd = per_game();
        let mut values = first_choices(&cmd);
        let ids = |v: &HashMap<String, String>| -> Vec<String> {
            shown_options(&cmd, v)
                .iter()
                .map(|o| o.id.clone())
                .collect()
        };
        assert_eq!(ids(&values), ["game", "players"]);
        values.insert("game".into(), "cod".into());
        assert_eq!(ids(&values), ["game", "kills"]);
    }

    #[test]
    fn a_field_filled_in_and_then_hidden_is_not_sent() {
        let cmd = per_game();
        let mut values = first_choices(&cmd);
        values.insert("players".into(), "4".into());
        values.insert("game".into(), "cod".into());
        let args = build_args(&cmd, &values, &HashMap::new()).unwrap();
        assert_eq!(args.len(), 1, "only the game: players belongs to smash");
    }

    #[test]
    fn a_person_is_found_by_name_or_either_spelling_of_their_key() {
        let p = Person {
            pubkey: "ab12".repeat(16),
            name: "Pepe Argento".into(),
            npub: "npub1xyz".into(),
        };
        for needle in ["", "pepe", " ARGENTO ", "ab12ab", "npub1x"] {
            assert!(person_matches(&p, needle), "{needle:?}");
        }
        for needle in ["moni", "12ab", "xyz"] {
            assert!(!person_matches(&p, needle), "{needle:?}");
        }
    }

    #[test]
    fn an_empty_optional_field_is_left_out() {
        let args = build_args(&cmd(), &first_choices(&cmd()), &HashMap::new()).unwrap();
        assert_eq!(args.len(), 1);
    }
}
