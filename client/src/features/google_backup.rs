use dioxus::prelude::*;
use zeroize::Zeroizing;

use crate::google_drive;
use crate::identity::{Identity, IdentitySource};
use crate::identity_backup::{self, Summary};

const INPUT: &str = "w-full bg-transparent border border-[var(--edge)] rounded-lg px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const SMALL_BUTTON: &str = "text-[10px] uppercase tracking-wider border border-[var(--edge)] rounded-md px-2.5 py-1 transition-colors disabled:opacity-30 disabled:cursor-not-allowed";
const PRIMARY_BUTTON: &str = "dxf-cta w-full py-2.5 rounded-xl text-sm transition-all disabled:opacity-30 disabled:cursor-not-allowed";

fn waiting_text() -> String {
    if cfg!(target_os = "android") {
        crate::i18n::text("Finish signing in with Google…", &[])
    } else {
        crate::i18n::text("Finish signing in with Google in your browser…", &[])
    }
}

fn done_page() -> String {
    crate::i18n::text(
        "Done. You can close this tab and go back to Discordia.",
        &[],
    )
}

#[derive(Clone, PartialEq)]
enum Backup {
    Idle,
    Form,
    Working,
    Done,
    Failed(String),
}

async fn back_up(
    identity: Identity,
    password: Option<Zeroizing<String>>,
    done_page: String,
) -> Result<(), String> {
    let name = identity_backup::file_name(&identity.pubkey);
    let content = tokio::task::spawn_blocking(move || {
        identity_backup::seal(&identity, password.as_ref().map(|p| p.as_str()))
    })
    .await
    .map_err(|_| "Couldn't prepare the backup.".to_string())??;
    let session = google_drive::sign_in(done_page).await?;
    session.put(&name, &content).await
}

/// On the account card. Unchecked is the one-click path; the box is there for
/// whoever does not want the Google account alone to be the identity.
#[component]
pub fn GoogleBackup(identity: Identity) -> Element {
    let mut phase = use_signal(|| Backup::Idle);
    let mut protect = use_signal(|| false);
    let mut password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut task = use_signal(|| None::<dioxus::core::Task>);

    if !google_drive::available() {
        return rsx! {};
    }
    let has_phrase = matches!(identity.source, IdentitySource::Phrase(_));
    let mismatch = protect() && !confirm().is_empty() && password() != confirm();
    let too_short =
        protect() && !password().is_empty() && !identity_backup::password_acceptable(&password());
    let ready = !protect()
        || (identity_backup::password_acceptable(&password()) && password() == confirm());

    rsx! {
        div { class: "space-y-1.5",
            div { class: "flex items-center gap-2",
                span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                    {crate::i18n::text("Google Drive backup", &[])}
                }
                span { class: "flex-1" }
                if matches!(phase(), Backup::Idle | Backup::Done) {
                    button {
                        r#type: "button",
                        class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] uppercase tracking-wider",
                        onclick: move |_| phase.set(Backup::Form),
                        {crate::i18n::text("Back up", &[])}
                    }
                }
            }
            match phase() {
                Backup::Idle => rsx! {
                    div { class: "text-[10px] text-[var(--text-dim)]",
                        {crate::i18n::text("Keep a copy in your Google Drive, in a folder only Discordia can see. Sign in with Google on another device to get it back.", &[])}
                    }
                },
                Backup::Form => rsx! {
                    div { class: "space-y-2 border border-[var(--edge)] rounded-xl p-3",
                        style: "background: var(--bg2);",
                        label { class: "flex items-center gap-2 cursor-pointer",
                            input {
                                r#type: "checkbox",
                                class: "accent-[var(--accent)]",
                                checked: protect(),
                                onchange: move |e| protect.set(e.checked()),
                            }
                            span { class: "text-[11px] text-[var(--text)]",
                                {crate::i18n::text("Add extra password protection", &[])}
                            }
                        }
                        if protect() {
                            input {
                                class: INPUT,
                                r#type: "password",
                                autocomplete: "new-password",
                                placeholder: crate::i18n::text("Backup password", &[]),
                                value: "{password}",
                                oninput: move |e| password.set(e.value()),
                            }
                            input {
                                class: INPUT,
                                r#type: "password",
                                autocomplete: "new-password",
                                placeholder: crate::i18n::text("Repeat the password", &[]),
                                value: "{confirm}",
                                oninput: move |e| confirm.set(e.value()),
                            }
                            if too_short {
                                p { class: "text-[10px] text-[var(--danger)]",
                                    {crate::i18n::text("At least {0} characters.", &[identity_backup::MIN_PASSWORD_CHARS.to_string()])}
                                }
                            }
                            if mismatch {
                                p { class: "text-[10px] text-[var(--danger)]",
                                    {crate::i18n::text("The passwords don't match.", &[])}
                                }
                            }
                            p { class: "text-[10px] text-[var(--text-dim)]",
                                if has_phrase {
                                    {crate::i18n::text("You'll need this password to restore from Google. If you forget it, your recovery phrase still works.", &[])}
                                } else {
                                    {crate::i18n::text("You'll need this password to restore from Google. If you forget it, this backup can't be opened.", &[])}
                                }
                            }
                        } else {
                            p { class: "text-[10px] text-[var(--warn)]",
                                {crate::i18n::text("Without it, anyone who gets into this Google account can sign in as you.", &[])}
                            }
                        }
                        div { class: "flex justify-end gap-2 pt-1",
                            button {
                                r#type: "button",
                                class: "{SMALL_BUTTON} text-[var(--text-muted)] hover:text-[var(--text)]",
                                onclick: move |_| {
                                    password.set(String::new());
                                    confirm.set(String::new());
                                    phase.set(Backup::Idle);
                                },
                                {crate::i18n::text("Cancel", &[])}
                            }
                            button {
                                r#type: "button",
                                class: "{SMALL_BUTTON} text-[var(--accent)] hover:text-[var(--accent-strong)]",
                                disabled: !ready,
                                onclick: move |_| {
                                    let chosen = protect().then(|| Zeroizing::new(password()));
                                    password.set(String::new());
                                    confirm.set(String::new());
                                    phase.set(Backup::Working);
                                    let identity = identity.clone();
                                    let page = done_page();
                                    task.set(Some(spawn(async move {
                                        match back_up(identity, chosen, page).await {
                                            Ok(()) => phase.set(Backup::Done),
                                            Err(e) => phase.set(Backup::Failed(e)),
                                        }
                                        task.set(None);
                                    })));
                                },
                                {crate::i18n::text("Back up", &[])}
                            }
                        }
                    }
                },
                Backup::Working => rsx! {
                    div { class: "flex items-center gap-2",
                        p { class: "flex-1 text-[10px] text-[var(--text-muted)]",
                            {waiting_text()}
                        }
                        button {
                            r#type: "button",
                            class: "{SMALL_BUTTON} text-[var(--text-muted)] hover:text-[var(--text)]",
                            onclick: move |_| {
                                if let Some(running) = task.take() {
                                    running.cancel();
                                }
                                phase.set(Backup::Idle);
                            },
                            {crate::i18n::text("Cancel", &[])}
                        }
                    }
                },
                Backup::Done => rsx! {
                    div { class: "text-[10px] text-[var(--up)]",
                        {crate::i18n::text("Backed up to Google Drive.", &[])}
                    }
                },
                Backup::Failed(e) => rsx! {
                    div { class: "flex items-center gap-2",
                        p { class: "flex-1 text-[10px] text-[var(--danger)]", {crate::i18n::message(&e)} }
                        button {
                            r#type: "button",
                            class: "{SMALL_BUTTON} text-[var(--accent)] hover:text-[var(--accent-strong)]",
                            onclick: move |_| phase.set(Backup::Form),
                            {crate::i18n::text("Try again", &[])}
                        }
                    }
                },
            }
        }
    }
}

#[derive(Clone)]
struct Found {
    summary: Summary,
    content: Zeroizing<String>,
}

#[derive(Clone)]
enum Restore {
    Working,
    Empty,
    Choosing(Vec<Found>),
    Unlock(Found),
    Opening,
    Failed(String),
}

const MAX_BACKUPS: usize = 20;

async fn fetch(done_page: String) -> Result<Vec<Found>, String> {
    let session = google_drive::sign_in(done_page).await?;
    let mut found = Vec::new();
    let remotes = session.list().await?;
    for remote in remotes
        .iter()
        .filter(|r| identity_backup::is_backup_name(&r.name))
        .take(MAX_BACKUPS)
    {
        let content = session
            .download(remote, identity_backup::MAX_BYTES)
            .await
            .map(Zeroizing::new);
        match content.and_then(|c| identity_backup::summary(&c).map(|s| (s, c))) {
            Ok((summary, content)) => found.push(Found { summary, content }),
            Err(error) => tracing::warn!(%error, "a file in the Drive app folder was skipped"),
        }
    }
    found.sort_by(|a, b| {
        a.summary
            .display_name
            .to_lowercase()
            .cmp(&b.summary.display_name.to_lowercase())
    });
    Ok(found)
}

async fn open(found: Found, password: Option<Zeroizing<String>>) -> Result<Identity, String> {
    tokio::task::spawn_blocking(move || {
        let identity =
            identity_backup::open(&found.content, password.as_ref().map(|p| p.as_str()))?;
        identity.save()?;
        Ok(identity)
    })
    .await
    .map_err(|_| "Couldn't open the backup.".to_string())?
}

async fn choose(found: Found, mut phase: Signal<Restore>, on_done: EventHandler<Identity>) {
    if found.summary.protected {
        phase.set(Restore::Unlock(found));
        return;
    }
    phase.set(Restore::Opening);
    match open(found, None).await {
        Ok(identity) => on_done.call(identity),
        Err(e) => phase.set(Restore::Failed(e)),
    }
}

/// The setup screen's way back in. One backup opens straight away; it asks
/// only when there is a choice to make or a password to type.
#[component]
pub fn GoogleSignIn(on_done: EventHandler<Identity>) -> Element {
    let mut phase = use_signal(|| Restore::Working);
    let mut password = use_signal(String::new);
    let mut unlock_error = use_signal(|| None::<String>);

    let mut start = move || {
        phase.set(Restore::Working);
        let page = done_page();
        spawn(async move {
            match fetch(page).await {
                Ok(found) if found.is_empty() => phase.set(Restore::Empty),
                Ok(mut found) if found.len() == 1 => choose(found.remove(0), phase, on_done).await,
                Ok(found) => phase.set(Restore::Choosing(found)),
                Err(e) => phase.set(Restore::Failed(e)),
            }
        });
    };
    use_hook(start);

    rsx! {
        div { class: "space-y-3",
            match phase() {
                Restore::Working => rsx! {
                    p { class: "text-sm text-[var(--text-muted)]",
                        {waiting_text()}
                    }
                },
                Restore::Opening => rsx! {
                    p { class: "text-sm text-[var(--text-muted)]",
                        {crate::i18n::text("Opening your identity…", &[])}
                    }
                },
                Restore::Empty => rsx! {
                    p { class: "text-sm text-[var(--text-muted)]",
                        {crate::i18n::text("No Discordia identities are backed up in that Google account.", &[])}
                    }
                    button {
                        r#type: "button",
                        class: PRIMARY_BUTTON,
                        onclick: move |_| start(),
                        if cfg!(target_os = "android") {
                            {crate::i18n::text("Try again", &[])}
                        } else {
                            {crate::i18n::text("Try another Google account", &[])}
                        }
                    }
                },
                Restore::Choosing(found) => rsx! {
                    p { class: "text-sm text-[var(--text-muted)]",
                        {crate::i18n::text("Choose the identity to sign in with.", &[])}
                    }
                    for item in found {
                        BackupRow {
                            key: "{item.summary.pubkey}",
                            summary: item.summary.clone(),
                            onclick: move |_| {
                                let item = item.clone();
                                spawn(async move { choose(item, phase, on_done).await });
                            },
                        }
                    }
                },
                Restore::Unlock(found) => {
                    let name = found.summary.display_name.clone();
                    let mut submit = move |found: Found| {
                        let typed = Zeroizing::new(password());
                        password.set(String::new());
                        unlock_error.set(None);
                        phase.set(Restore::Opening);
                        spawn(async move {
                            match open(found.clone(), Some(typed)).await {
                                Ok(identity) => on_done.call(identity),
                                Err(e) => {
                                    unlock_error.set(Some(e));
                                    phase.set(Restore::Unlock(found));
                                }
                            }
                        });
                    };
                    let on_enter = found.clone();
                    rsx! {
                        p { class: "text-sm text-[var(--text-muted)]",
                            {crate::i18n::text("{0} is protected with a backup password.", &[name])}
                        }
                        input {
                            class: INPUT,
                            r#type: "password",
                            autocomplete: "current-password",
                            autofocus: true,
                            placeholder: crate::i18n::text("Backup password", &[]),
                            value: "{password}",
                            oninput: move |e| password.set(e.value()),
                            onkeydown: move |e| {
                                if e.key().to_string() == "Enter" && !password().is_empty() {
                                    submit(on_enter.clone());
                                }
                            },
                        }
                        if let Some(e) = unlock_error() {
                            p { class: "text-xs text-[var(--danger)]", {crate::i18n::message(&e)} }
                        }
                        button {
                            r#type: "button",
                            class: PRIMARY_BUTTON,
                            disabled: password().is_empty(),
                            onclick: move |_| submit(found.clone()),
                            {crate::i18n::text("Unlock", &[])}
                        }
                    }
                },
                Restore::Failed(e) => rsx! {
                    div { class: "text-xs text-[var(--danger)] border border-[var(--danger)]/40 rounded-lg px-3 py-2",
                        {crate::i18n::message(&e)}
                    }
                    button {
                        r#type: "button",
                        class: PRIMARY_BUTTON,
                        onclick: move |_| start(),
                        {crate::i18n::text("Try again", &[])}
                    }
                },
            }
        }
    }
}

#[component]
fn BackupRow(summary: Summary, onclick: EventHandler<()>) -> Element {
    let tag = crate::identity::discriminator(&summary.pubkey).to_string();
    rsx! {
        button {
            r#type: "button",
            class: "w-full flex items-center gap-3 text-left border border-[var(--edge)] rounded-xl px-3 py-2.5 hover:border-[var(--accent)] transition-colors",
            onclick: move |_| onclick.call(()),
            super::profiles::Avatar {
                pubkey: summary.pubkey.clone(),
                name: summary.display_name.clone(),
                size: "w-8 h-8",
                text: "text-sm",
            }
            span { class: "flex-1 min-w-0 truncate text-sm text-[var(--text)]",
                "{summary.display_name}"
                span { class: "text-[var(--text-dim)] font-mono text-xs", " #{tag}" }
            }
            if summary.protected {
                span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                    {crate::i18n::text("password", &[])}
                }
            }
        }
    }
}
