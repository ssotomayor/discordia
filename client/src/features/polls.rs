use dioxus::prelude::*;

use crate::protocol::{ClientMessage, Id, Permission, Poll};
use crate::state::{use_app_state, use_gateway};

/// The channel the poll dialog is open for. Mounted at the workspace root, not
/// in the chat panel, whose stacking context would trap it (trap 22).
#[derive(Clone, Copy)]
pub struct PollTarget(pub Signal<Option<Id>>);

const DURATIONS: &[(Option<u32>, &str)] = &[
    (Some(60 * 60), "1 hour"),
    (Some(4 * 60 * 60), "4 hours"),
    (Some(24 * 60 * 60), "1 day"),
    (Some(3 * 24 * 60 * 60), "3 days"),
    (Some(7 * 24 * 60 * 60), "1 week"),
    (None, "Until I end it"),
];
const DEFAULT_DURATION: usize = 2;

const FIELD: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";

#[component]
pub fn PollDialog() -> Element {
    let Some(PollTarget(mut target)) = try_use_context::<PollTarget>() else {
        return rsx! {};
    };
    let Some(channel_id) = target() else {
        return rsx! {};
    };
    rsx! {
        PollForm { key: "{channel_id}", channel_id, on_close: move |_| target.set(None) }
    }
}

#[component]
fn PollForm(channel_id: Id, on_close: EventHandler<()>) -> Element {
    let gateway = use_gateway();
    let mut question = use_signal(String::new);
    let mut answers = use_signal(|| vec![String::new(), String::new()]);
    let mut anonymous = use_signal(|| false);
    let mut multiple = use_signal(|| false);
    let mut changeable = use_signal(|| true);
    let mut duration = use_signal(|| DEFAULT_DURATION);
    let mut error = use_signal(|| None::<String>);

    let submit = move |_| {
        let filled: Vec<String> = answers
            .read()
            .iter()
            .filter(|a| !a.trim().is_empty())
            .cloned()
            .collect();
        match crate::protocol::validate_poll(&question.read(), &filled) {
            Err(message) => error.set(Some(message)),
            Ok((question, options)) => {
                gateway.send(ClientMessage::CreatePoll {
                    channel_id,
                    question,
                    options,
                    anonymous: anonymous(),
                    multiple: multiple(),
                    changeable: changeable(),
                    duration_secs: DURATIONS[duration()].0,
                });
                on_close.call(());
            }
        }
    };
    let count = answers.read().len();

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                style: "width: min(26rem, calc(100vw - 2rem)); max-height: calc(100vh - 2rem);",
                role: "dialog",
                aria_label: crate::i18n::text("Create a poll", &[]),
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)]",
                    h3 { class: "text-sm font-medium text-[var(--text)]", {crate::i18n::text("Create a poll", &[])} }
                }
                div { class: "px-4 py-3 flex flex-col gap-3 overflow-y-auto",
                    input {
                        class: FIELD,
                        r#type: "text",
                        maxlength: "{crate::protocol::POLL_QUESTION_CHARS}",
                        placeholder: crate::i18n::text("Ask a question", &[]),
                        value: "{question}",
                        autofocus: true,
                        oninput: move |e| { question.set(e.value()); error.set(None); },
                    }
                    div { class: "flex flex-col gap-2",
                        for (index, answer) in answers.read().iter().cloned().enumerate() {
                            div { key: "{index}", class: "flex items-center gap-2",
                                input {
                                    class: FIELD,
                                    r#type: "text",
                                    maxlength: "{crate::protocol::POLL_OPTION_CHARS}",
                                    placeholder: crate::i18n::text("Answer {0}", &[(index + 1).to_string()]),
                                    value: "{answer}",
                                    oninput: move |e| {
                                        if let Some(slot) = answers.write().get_mut(index) {
                                            *slot = e.value();
                                        }
                                        error.set(None);
                                    },
                                }
                                if count > crate::protocol::POLL_MIN_OPTIONS {
                                    button {
                                        r#type: "button",
                                        class: "w-7 h-7 shrink-0 flex items-center justify-center rounded-md text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                        title: crate::i18n::text("Remove answer", &[]),
                                        onclick: move |_| { answers.write().remove(index); },
                                        span { class: "block w-3.5 h-3.5", dangerous_inner_html: super::icons::CLOSE }
                                    }
                                }
                            }
                        }
                        if count < crate::protocol::POLL_MAX_OPTIONS {
                            button {
                                r#type: "button",
                                class: "self-start flex items-center gap-1.5 px-2 h-7 rounded-md text-[11.5px] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                                onclick: move |_| answers.write().push(String::new()),
                                span { class: "block w-3.5 h-3.5", dangerous_inner_html: super::icons::PLUS }
                                {crate::i18n::text("Add answer", &[])}
                            }
                        }
                    }
                    div { class: "flex flex-col gap-1.5",
                        Check { checked: anonymous(), on_change: move |on| anonymous.set(on),
                            label: crate::i18n::text("Anonymous", &[]),
                            hint: crate::i18n::text("Nobody sees who voted for what.", &[]),
                        }
                        Check { checked: multiple(), on_change: move |on| multiple.set(on),
                            label: crate::i18n::text("Multiple choice", &[]),
                            hint: crate::i18n::text("Voters may pick more than one answer.", &[]),
                        }
                        Check { checked: changeable(), on_change: move |on| changeable.set(on),
                            label: crate::i18n::text("Allow changing votes", &[]),
                            hint: crate::i18n::text("Voters may change or take back their vote while the poll is open.", &[]),
                        }
                    }
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                        span { class: "flex-1", {crate::i18n::text("Ends after", &[])} }
                        select {
                            class: "bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-md px-2 py-1 text-xs text-[var(--text)]",
                            onchange: move |e| duration.set(e.value().parse().unwrap_or(DEFAULT_DURATION)),
                            for (index, (_, label)) in DURATIONS.iter().enumerate() {
                                option { value: "{index}", selected: duration() == index, {crate::i18n::label(label)} }
                            }
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
                        onclick: move |_| on_close.call(()),
                        {crate::i18n::text("Cancel", &[])}
                    }
                    button {
                        r#type: "button",
                        class: "px-3 py-1 rounded border border-[var(--accent)] text-[11px] text-[var(--accent)] hover:bg-[var(--accent-soft)] transition-colors",
                        onclick: submit,
                        {crate::i18n::text("Post poll", &[])}
                    }
                }
            }
        }
    }
}

#[component]
fn Check(checked: bool, on_change: EventHandler<bool>, label: String, hint: String) -> Element {
    rsx! {
        label { class: "flex items-start gap-2 cursor-pointer select-none",
            input {
                r#type: "checkbox",
                class: "mt-0.5 accent-[var(--accent)]",
                checked,
                onchange: move |e| on_change.call(e.checked()),
            }
            span { class: "flex flex-col",
                span { class: "text-xs text-[var(--text)]", {label} }
                span { class: "text-[10.5px] text-[var(--text-dim)]", {hint} }
            }
        }
    }
}

/// What a press on `option` makes the voter's choices.
pub(crate) fn next_choices(poll: &Poll, option: u8) -> Vec<u8> {
    let mut next = poll.mine.clone();
    if poll.multiple {
        match next.iter().position(|&c| c == option) {
            Some(at) => {
                next.remove(at);
            }
            None => {
                next.push(option);
                next.sort_unstable();
            }
        }
    } else if next == [option] {
        next.clear();
    } else {
        next = vec![option];
    }
    next
}

/// Shows a vote before the server confirms it; its `PollUpdate` then replaces the counts.
pub(crate) fn choose(poll: &mut Poll, me: &str, next: Vec<u8>) {
    for &old in &poll.mine {
        if let Some(count) = poll.counts.get_mut(usize::from(old)) {
            *count = count.saturating_sub(1);
        }
    }
    for list in &mut poll.voters {
        list.retain(|voter| voter != me);
    }
    for &new in &next {
        if let Some(count) = poll.counts.get_mut(usize::from(new)) {
            *count += 1;
        }
        if !poll.anonymous
            && let Some(list) = poll.voters.get_mut(usize::from(new))
        {
            list.push(me.to_string());
        }
    }
    poll.mine = next;
}

fn time_left(until: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (until - now).num_seconds().max(0);
    match secs {
        0..60 => crate::i18n::text("Ends in under a minute", &[]),
        60..3600 => crate::i18n::text("Ends in {0} min", &[(secs / 60).to_string()]),
        3600..86_400 => crate::i18n::text("Ends in {0} h", &[(secs / 3600).to_string()]),
        _ => crate::i18n::text("Ends in {0} d", &[(secs / 86_400).to_string()]),
    }
}

#[component]
pub fn PollCard(
    channel_id: Id,
    message_id: Id,
    question: String,
    poll: Poll,
    author: String,
) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut tick = use_signal(|| 0u32);
    let closes_at = poll.closes_at;
    // Re-renders once the end time passes, so the card closes without a message.
    use_future(move || async move {
        let Some(at) = closes_at else { return };
        let wait = (at - chrono::Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait + std::time::Duration::from_millis(250)).await;
        tick += 1;
    });
    let _ = tick();

    let now = chrono::Utc::now();
    let open = poll.is_open(now);
    let s = state.read();
    let me = s
        .self_user
        .as_ref()
        .map(|u| u.pubkey.clone())
        .unwrap_or_default();
    let moderates = s
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .is_some_and(|c| s.can(c.guild_id, Permission::ManageMessages));
    let can_end = open && (author == me || moderates);
    let names: Vec<String> = poll
        .voters
        .iter()
        .map(|voters| {
            voters
                .iter()
                .map(|v| s.display_name(v))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect();
    drop(s);
    let total = poll.total();
    let locked = !open || (!poll.changeable && !poll.mine.is_empty());
    let status = if !open {
        crate::i18n::text("Poll ended", &[])
    } else if let Some(at) = poll.closes_at {
        time_left(at, now)
    } else {
        crate::i18n::text("Open until ended", &[])
    };

    let vote = {
        let gateway = gateway.clone();
        let poll = poll.clone();
        move |option: u8| {
            let next = next_choices(&poll, option);
            if !poll.changeable && !poll.mine.is_empty() {
                return;
            }
            if let Some(shown) = state
                .write()
                .messages
                .get_mut(&channel_id)
                .and_then(|msgs| msgs.iter_mut().find(|m| m.id == message_id))
                .and_then(|m| m.poll.as_mut())
            {
                choose(shown, &me, next.clone());
            }
            gateway.send(ClientMessage::VotePoll {
                channel_id,
                message_id,
                choices: next,
            });
        }
    };

    rsx! {
        div { class: "dxf-poll mt-1 w-full max-w-[28rem] flex flex-col gap-2 rounded-lg border border-[var(--border)] bg-[var(--panel2)] px-3 py-2.5",
            div { class: "text-sm font-semibold text-[var(--text)] break-words", "{question}" }
            div { class: "text-[10.5px] text-[var(--text-dim)]",
                if poll.multiple { {crate::i18n::text("Pick one or more", &[])} } else { {crate::i18n::text("Pick one", &[])} }
            }
            div { class: "flex flex-col gap-1.5", role: if poll.multiple { "group" } else { "radiogroup" },
                for (index, option) in poll.options.iter().cloned().enumerate() {
                    {
                        let choice = index as u8;
                        let count = poll.counts.get(index).copied().unwrap_or(0);
                        let percent = if total == 0 { 0 } else { count * 100 / total.max(1) };
                        let chosen = poll.mine.contains(&choice);
                        let who = names.get(index).cloned().unwrap_or_default();
                        let mut vote = vote.clone();
                        rsx! {
                            button {
                                key: "{index}",
                                r#type: "button",
                                class: if chosen {
                                    "relative w-full overflow-hidden rounded-md border border-[var(--accent)] text-left disabled:cursor-default"
                                } else {
                                    "relative w-full overflow-hidden rounded-md border border-[var(--border)] text-left hover:border-[var(--accent)] disabled:cursor-default disabled:hover:border-[var(--border)]"
                                },
                                disabled: locked,
                                role: if poll.multiple { "checkbox" } else { "radio" },
                                aria_checked: "{chosen}",
                                title: (!who.is_empty()).then_some(who),
                                onclick: move |_| vote(choice),
                                div {
                                    class: "absolute inset-y-0 left-0 bg-[var(--accent-soft)] transition-[width] duration-300",
                                    style: "width: {percent}%;",
                                }
                                div { class: "relative flex items-center gap-2 px-2.5 py-1.5 text-[12.5px]",
                                    span {
                                        class: if chosen { "w-4 h-4 shrink-0 flex items-center justify-center rounded-full bg-[var(--accent)] text-[var(--bg)]" } else { "w-4 h-4 shrink-0 rounded-full border border-[var(--edge-strong)]" },
                                        if chosen { span { class: "block w-3 h-3", dangerous_inner_html: super::icons::CHECK } }
                                    }
                                    span { class: "flex-1 min-w-0 break-words text-[var(--text)]", "{option}" }
                                    span { class: "shrink-0 tabular-nums text-[11px] text-[var(--text-muted)]", "{percent}% · {count}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "flex flex-wrap items-center gap-x-2 gap-y-1 text-[10.5px] text-[var(--text-dim)]",
                span { {crate::i18n::plural("{0} vote", "{0} votes", total as usize)} }
                span { "·" }
                span { "{status}" }
                if poll.anonymous {
                    span { "·" }
                    span { {crate::i18n::text("Anonymous", &[])} }
                }
                if open && !poll.changeable && poll.mine.is_empty() {
                    span { "·" }
                    span { {crate::i18n::text("Votes are final", &[])} }
                }
                if can_end {
                    div { class: "flex-1" }
                    button {
                        r#type: "button",
                        class: "px-2 h-6 rounded text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                        onclick: move |_| gateway.send(ClientMessage::ClosePoll { channel_id, message_id }),
                        {crate::i18n::text("End poll", &[])}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(multiple: bool, anonymous: bool) -> Poll {
        Poll {
            options: vec!["a".into(), "b".into(), "c".into()],
            multiple,
            anonymous,
            changeable: true,
            counts: vec![0, 0, 0],
            voters: if anonymous {
                Vec::new()
            } else {
                vec![Vec::new(); 3]
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_press_picks_moves_or_takes_back_a_single_choice() {
        let mut p = poll(false, false);
        assert_eq!(next_choices(&p, 1), vec![1]);
        p.mine = vec![1];
        assert_eq!(next_choices(&p, 2), vec![2]);
        assert_eq!(next_choices(&p, 1), Vec::<u8>::new());
    }

    #[test]
    fn a_press_toggles_one_answer_of_a_multiple_choice() {
        let mut p = poll(true, false);
        p.mine = vec![2];
        assert_eq!(next_choices(&p, 0), vec![0, 2]);
        p.mine = vec![0, 2];
        assert_eq!(next_choices(&p, 2), vec![0]);
    }

    #[test]
    fn an_early_vote_moves_counts_and_names_only_on_a_public_poll() {
        let mut p = poll(false, false);
        choose(&mut p, "me", vec![1]);
        assert_eq!(
            (p.counts.clone(), p.voters[1].clone()),
            (vec![0, 1, 0], vec!["me".to_string()])
        );
        choose(&mut p, "me", vec![2]);
        assert_eq!(p.counts, vec![0, 0, 1]);
        assert!(p.voters[1].is_empty());

        let mut hidden = poll(false, true);
        choose(&mut hidden, "me", vec![0]);
        assert_eq!(hidden.counts, vec![1, 0, 0]);
        assert!(hidden.voters.is_empty());
        assert_eq!(hidden.mine, vec![0]);
    }
}
