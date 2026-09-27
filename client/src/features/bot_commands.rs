use std::collections::HashMap;

use dioxus::prelude::*;

use crate::protocol::{
    ArgValue, BotCommand, ChannelKind, ClientMessage, CommandArg, Id, OptionKind,
};
use crate::state::{use_app_state, use_gateway};

const HEADING: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";
const FIELD: &str = "w-full bg-[var(--panel-solid)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors";

/// What the form holds, turned into what the server accepts. Runs the same
/// check the server does, so a mistake is named here instead of in a toast.
fn build_args(
    cmd: &BotCommand,
    values: &HashMap<String, String>,
    picked: &HashMap<String, Vec<String>>,
) -> Result<Vec<CommandArg>, String> {
    let mut args = Vec::new();
    for opt in &cmd.options {
        let raw = values.get(&opt.id).map(|v| v.trim()).unwrap_or("");
        let value = match &opt.kind {
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
        };
        if let Some(value) = value {
            args.push(CommandArg {
                id: opt.id.clone(),
                value,
            });
        }
    }
    crate::protocol::check_args(cmd, args)
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

/// The buttons on a bot's profile card. Hidden where the viewer lacks a
/// command's permission; the server refuses it regardless (trap 5).
#[component]
pub fn BotCommandsSection(bot_pubkey: String) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut open = use_signal(|| None::<String>);
    let mut values = use_signal(HashMap::<String, String>::new);
    let mut picked = use_signal(HashMap::<String, Vec<String>>::new);
    let mut filter = use_signal(String::new);
    let mut form_error = use_signal(|| None::<String>);

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
    let channel = s
        .selected_channel
        .and_then(|id| {
            s.channels
                .iter()
                .find(|c| c.id == id && c.guild_id == guild_id && c.kind == ChannelKind::Text)
        })
        .map(|c| (c.id, c.name.clone()));
    let people: Vec<(String, String)> = s
        .members_of(guild_id)
        .into_iter()
        .filter(|m| !m.bot)
        .map(|m| (m.user.pubkey.clone(), s.display_name(&m.user.pubkey)))
        .collect();
    drop(s);

    let usable = online && channel.is_some();
    let open_cmd = open
        .read()
        .as_ref()
        .and_then(|id| commands.iter().find(|c| &c.id == id).cloned());

    let press = {
        let gateway = gateway.clone();
        let bot_pubkey = bot_pubkey.clone();
        move |cmd: BotCommand, channel_id: Id, args: Vec<CommandArg>| {
            gateway.send(ClientMessage::InvokeCommand {
                guild_id,
                channel_id,
                bot_pubkey: bot_pubkey.clone(),
                command: cmd.id,
                args,
            });
            state.write().profile_card = None;
        }
    };

    rsx! {
        div { class: "mt-3",
            div { class: "flex items-center justify-between mb-1.5",
                span { class: HEADING, "Commands" }
                match (&channel, online) {
                    (_, false) => rsx! { span { class: "text-[10px] text-[var(--text-dim)]", "offline" } },
                    (None, true) => rsx! { span { class: "text-[10px] text-[var(--text-dim)]", "open a text channel to use them" } },
                    (Some((_, name)), true) => rsx! { span { class: "text-[10px] text-[var(--text-dim)]", "runs in #{name}" } },
                }
            }
            div { class: "flex flex-wrap gap-1.5",
                for cmd in commands.iter() {
                    {
                        let cmd = cmd.clone();
                        let active = open.read().as_deref() == Some(cmd.id.as_str());
                        let mut press = press.clone();
                        let channel_id = channel.as_ref().map(|(id, _)| *id);
                        rsx! {
                            button {
                                key: "{cmd.id}",
                                class: if active {
                                    "px-2.5 py-1 rounded-lg text-xs border border-[var(--accent)] text-[var(--accent)] bg-[var(--accent-soft)] transition-colors"
                                } else {
                                    "px-2.5 py-1 rounded-lg text-xs border border-[var(--border)] text-[var(--text)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors disabled:opacity-40"
                                },
                                disabled: !usable,
                                title: "{cmd.description}",
                                onclick: move |_| {
                                    let Some(channel_id) = channel_id else { return };
                                    if cmd.options.is_empty() {
                                        press(cmd.clone(), channel_id, Vec::new());
                                    } else if active {
                                        open.set(None);
                                    } else {
                                        values.set(first_choices(&cmd));
                                        picked.set(HashMap::new());
                                        filter.set(String::new());
                                        form_error.set(None);
                                        open.set(Some(cmd.id.clone()));
                                    }
                                },
                                if cmd.options.is_empty() { "{cmd.label}" } else { "{cmd.label} …" }
                            }
                        }
                    }
                }
            }
            if let (Some(cmd), Some((channel_id, _))) = (open_cmd, channel.clone()) {
                {
                    let run_cmd = cmd.clone();
                    let mut press = press.clone();
                    rsx! {
                        div { class: "mt-2 rounded-xl border border-[var(--edge)] p-3 flex flex-col gap-2.5", style: "background: var(--bg2);",
                            if !cmd.description.is_empty() {
                                div { class: "text-xs text-[var(--text-muted)]", "{cmd.description}" }
                            }
                            for opt in cmd.options.iter() {
                                {
                                    let id = opt.id.clone();
                                    let label = if opt.required { opt.label.clone() } else { format!("{} (optional)", opt.label) };
                                    let field = match opt.kind.clone() {
                                        OptionKind::Text { max_len } => rsx! {
                                            input {
                                                class: FIELD,
                                                maxlength: max_len as i64,
                                                value: values.read().get(&id).cloned().unwrap_or_default(),
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
                                                value: values.read().get(&id).cloned().unwrap_or_default(),
                                                oninput: move |e| { values.write().insert(id.clone(), e.value()); },
                                            }
                                        },
                                        OptionKind::Choice { choices } => rsx! {
                                            select {
                                                class: FIELD,
                                                value: values.read().get(&id).cloned().unwrap_or_default(),
                                                onchange: move |e| { values.write().insert(id.clone(), e.value()); },
                                                if !opt.required {
                                                    option { value: "", "—" }
                                                }
                                                for c in choices.iter() {
                                                    option { key: "{c.value}", value: "{c.value}", "{c.label}" }
                                                }
                                            }
                                        },
                                        OptionKind::Members { max, .. } => {
                                            let chosen = picked.read().get(&id).cloned().unwrap_or_default();
                                            let needle = filter.read().to_lowercase();
                                            let shown: Vec<(String, String)> = people
                                                .iter()
                                                .filter(|(pk, name)| chosen.contains(pk) || needle.is_empty() || name.to_lowercase().contains(&needle))
                                                .cloned()
                                                .collect();
                                            rsx! {
                                                input {
                                                    class: FIELD,
                                                    placeholder: "Search members",
                                                    value: "{filter}",
                                                    oninput: move |e| filter.set(e.value()),
                                                }
                                                div { class: "flex flex-wrap gap-1 max-h-32 overflow-y-auto",
                                                    for (pk, name) in shown {
                                                        {
                                                            let on = chosen.contains(&pk);
                                                            let id = id.clone();
                                                            rsx! {
                                                                button {
                                                                    key: "{pk}",
                                                                    class: if on {
                                                                        "px-2 py-0.5 rounded-md text-xs border border-[var(--accent)] text-[var(--accent)] bg-[var(--accent-soft)]"
                                                                    } else {
                                                                        "px-2 py-0.5 rounded-md text-xs border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)]"
                                                                    },
                                                                    onclick: move |_| {
                                                                        let mut all = picked.write();
                                                                        let list = all.entry(id.clone()).or_default();
                                                                        if let Some(i) = list.iter().position(|k| k == &pk) {
                                                                            list.remove(i);
                                                                        } else if max == 1 {
                                                                            *list = vec![pk.clone()];
                                                                        } else if list.len() < max as usize {
                                                                            list.push(pk.clone());
                                                                        }
                                                                    },
                                                                    "{name}"
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    };
                                    rsx! {
                                        div { key: "{opt.id}", class: "flex flex-col gap-1",
                                            span { class: HEADING, "{label}" }
                                            if !opt.description.is_empty() {
                                                span { class: "text-[11px] text-[var(--text-dim)]", "{opt.description}" }
                                            }
                                            {field}
                                        }
                                    }
                                }
                            }
                            if let Some(err) = form_error() {
                                div { class: "text-xs text-[var(--danger)]", "{err}" }
                            }
                            div { class: "flex gap-2 justify-end",
                                button {
                                    class: "px-3 py-1 rounded-lg text-xs text-[var(--text-muted)] hover:text-[var(--text)]",
                                    onclick: move |_| open.set(None),
                                    "Cancel"
                                }
                                button {
                                    class: "dxf-cta px-3 py-1 rounded-lg text-xs",
                                    onclick: move |_| {
                                        match build_args(&run_cmd, &values.read(), &picked.read()) {
                                            Ok(args) => {
                                                open.set(None);
                                                press(run_cmd.clone(), channel_id, args);
                                            }
                                            Err(e) => form_error.set(Some(e)),
                                        }
                                    },
                                    "Run"
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
                    span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]", "only you can see this" }
                    button {
                        class: "ml-auto text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)]",
                        onclick: move |_| state.write().command_notes.retain(|n| n.invocation_id != id),
                        "Dismiss"
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

    #[test]
    fn an_empty_optional_field_is_left_out() {
        let args = build_args(&cmd(), &first_choices(&cmd()), &HashMap::new()).unwrap();
        assert_eq!(args.len(), 1);
    }
}
