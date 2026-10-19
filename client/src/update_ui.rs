use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use dioxus::prelude::*;
use futures_util::StreamExt;

use crate::version::Update;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Check {
    #[default]
    Idle,
    Searching,
    Current,
    Local,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Phase {
    #[default]
    Offered,
    Downloading,
    Ready(PathBuf),
    Leaving(PathBuf),
    Installing,
    Finished,
    Opened,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub offer: Option<Update>,
    pub check: Check,
    pub phase: Phase,
    pub progress: (u64, Option<u64>),
    pub checked: Option<SystemTime>,
    pub dismissed: bool,
    pub in_call: bool,
}

impl State {
    fn can_check(&self) -> bool {
        self.check != Check::Searching
            && matches!(
                self.phase,
                Phase::Offered | Phase::Failed(_) | Phase::Opened
            )
    }

    fn checked(&mut self, result: Result<crate::version::CheckResult, String>) {
        self.checked = Some(SystemTime::now());
        match result {
            Ok(crate::version::CheckResult::Available(offer)) => {
                if self.offer.as_ref() != Some(&offer) {
                    self.dismissed = false;
                    self.phase = Phase::Offered;
                }
                self.offer = Some(offer);
                self.check = Check::Idle;
            }
            Ok(crate::version::CheckResult::Current) => self.check = Check::Current,
            Ok(crate::version::CheckResult::Local) => self.check = Check::Local,
            Err(message) => self.check = Check::Failed(message),
        }
    }
}

enum Command {
    Check,
    Download(Update),
    Install(PathBuf),
}

#[derive(Clone, Copy)]
pub struct Updates {
    pub state: Signal<State>,
    pub open_request: Signal<u64>,
    commands: Coroutine<Command>,
}

impl Updates {
    pub fn check(mut self) {
        if !self.state.peek().can_check() {
            return;
        }
        self.state.write().check = Check::Searching;
        self.commands.send(Command::Check);
    }

    fn download(mut self) {
        let offer = {
            let current = self.state.peek();
            if current.check == Check::Searching
                || !matches!(current.phase, Phase::Offered | Phase::Failed(_))
            {
                return;
            }
            current
                .offer
                .clone()
                .filter(|offer| offer.download.is_some())
        };
        if let Some(offer) = offer {
            let mut state = self.state.write();
            state.phase = Phase::Downloading;
            state.progress = (0, None);
            state.dismissed = false;
            drop(state);
            self.commands.send(Command::Download(offer));
        }
    }

    fn request_install(mut self) {
        let path = {
            let state = self.state.peek();
            if state.in_call {
                return;
            }
            match &state.phase {
                Phase::Ready(path) => path.clone(),
                _ => return,
            }
        };
        self.state.write().phase = Phase::Leaving(path);
    }

    pub fn install_after_disconnect(mut self) {
        if self.state.peek().in_call {
            return;
        }
        let path = match &self.state.peek().phase {
            Phase::Leaving(path) => path.clone(),
            _ => return,
        };
        self.state.write().phase = Phase::Installing;
        self.commands.send(Command::Install(path));
    }

    pub fn prevents_close(self) -> bool {
        matches!(
            self.state.read().phase,
            Phase::Leaving(_) | Phase::Installing | Phase::Finished
        )
    }

    pub fn open(mut self) {
        let next = self.open_request.peek().wrapping_add(1);
        self.open_request.set(next);
    }
}

pub fn use_updates() -> Updates {
    let mut state = use_hook(|| Signal::new_in_scope(State::default(), ScopeId::ROOT));
    let open_request = use_hook(|| Signal::new_in_scope(0_u64, ScopeId::ROOT));
    use_drop(move || {
        state.manually_drop();
        open_request.manually_drop();
    });
    let commands = use_coroutine(move |mut rx: UnboundedReceiver<Command>| async move {
        while let Some(command) = rx.next().await {
            match command {
                Command::Check => {
                    let result = crate::version::check_for_update().await;
                    state.write().checked(result);
                }
                Command::Download(offer) => {
                    let Some(download) = offer.download else {
                        continue;
                    };
                    let mut painted = 0;
                    let result = super::fetch_verified(
                        &offer.tag,
                        &download.asset,
                        &download.asset_url,
                        &download.signature_url,
                        move |got, total| {
                            if super::should_paint(got, painted, total) {
                                painted = got;
                                state.write().progress = (got, total);
                            }
                        },
                    )
                    .await;
                    state.write().phase = match result {
                        Ok(path) => Phase::Ready(path),
                        Err(error) => Phase::Failed(error),
                    };
                }
                Command::Install(path) => {
                    let result = tokio::task::spawn_blocking(move || {
                        super::perform(&super::plan_install(path))
                    })
                    .await;
                    match result {
                        Ok(Ok(super::Applied::Opened)) => state.write().phase = Phase::Opened,
                        Ok(Ok(_)) => {
                            state.write().phase = Phase::Finished;
                            tokio::time::sleep(Duration::from_millis(900)).await;
                            std::process::exit(0);
                        }
                        Ok(Err(error)) => state.write().phase = Phase::Failed(error),
                        Err(error) => state.write().phase = Phase::Failed(error.to_string()),
                    }
                }
            }
        }
    });
    let updates = Updates {
        state,
        open_request,
        commands,
    };
    use_context_provider(|| updates);
    use_effect(move || updates.check());
    updates
}

#[component]
pub fn UpdateBanner() -> Element {
    let mut updates = use_context::<Updates>();
    let state = updates.state.read();
    if state.offer.is_none() || state.dismissed {
        return rsx! {};
    }
    let title = match state.phase {
        Phase::Downloading => "Downloading update…",
        Phase::Ready(_) => "Ready to install",
        Phase::Leaving(_) | Phase::Installing | Phase::Finished => "Updating Discordia…",
        Phase::Failed(_) => "Update failed",
        _ => "A new update is available",
    };
    let detail = match state.phase {
        Phase::Downloading => "You can keep using Discordia.",
        Phase::Ready(_) => "Restart when it suits you.",
        _ => "Improvements and fixes for Discordia.",
    };
    rsx! {
        section { class: "dxf-update-banner", role: "status", "aria-live": "polite",
            UpdateStatusIcon { kind: "available" }
            div { class: "dxf-update-banner-copy", strong { {crate::i18n::label(title)} } p { {crate::i18n::label(detail)} } }
            button { class: "dxf-update-primary", onclick: move |_| updates.open(), {crate::i18n::text("View update", &[])} }
            button { class: "dxf-update-dismiss", aria_label: crate::i18n::text("Dismiss update notice", &[]), onclick: move |_| updates.state.write().dismissed = true, "×" }
        }
    }
}

#[component]
fn UpdateStatusIcon(kind: &'static str) -> Element {
    rsx! {
        span { class: "dxf-update-icon", "data-state": kind, "aria-hidden": "true",
            svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                match kind {
                    "success" => rsx! { path { d: "M5 12l4 4L19 6" } },
                    "error" => rsx! { path { d: "M12 7v6M12 17h.01" } circle { cx: "12", cy: "12", r: "9" } },
                    "busy" => rsx! { path { d: "M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5" } },
                    "local" => rsx! { path { d: "M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16" } },
                    _ => rsx! { path { d: "M12 3v12m-5-5 5 5 5-5M3 15v5h18v-5" } },
                }
            }
        }
    }
}

#[component]
pub fn UpdatePanel() -> Element {
    let updates = use_context::<Updates>();
    let state = updates.state.read().clone();
    let title = match &state.phase {
        Phase::Downloading => "Downloading update…",
        Phase::Ready(_) => "Ready to install",
        Phase::Leaving(_) => "Closing your session…",
        Phase::Installing => "Installing update…",
        Phase::Finished => "Installed — restarting Discordia.",
        Phase::Opened => "Verified — finish in the installer window.",
        Phase::Failed(_) => "Update failed",
        _ if state.offer.is_some() => "A new version is available",
        _ if state.check == Check::Searching => "Checking for updates…",
        _ if matches!(state.check, Check::Failed(_)) => "Could not check for updates",
        _ if state.check == Check::Current => "You're up to date",
        _ if state.check == Check::Local => "Local build",
        _ => "Keep Discordia up to date",
    };
    let detail = match &state.phase {
        Phase::Ready(_) => "The app restarts only when you choose.",
        Phase::Downloading => "You can keep using Discordia.",
        Phase::Leaving(_) | Phase::Installing | Phase::Finished => {
            "Do not close Discordia while the update is installed."
        }
        _ if state.check == Check::Local && state.offer.is_none() => {
            "This is a local build. Automatic updates are available in published releases."
        }
        _ if state.offer.is_some() => "Download now and restart when it suits you.",
        _ if state.check == Check::Current => "You're using the latest available version.",
        _ if matches!(state.check, Check::Failed(_)) => "Check your connection and try again.",
        _ => "Check for a new version whenever you want.",
    };
    let kind = match &state.phase {
        Phase::Failed(_) => "error",
        Phase::Downloading | Phase::Leaving(_) | Phase::Installing => "busy",
        Phase::Ready(_) | Phase::Finished | Phase::Opened => "success",
        _ if state.check == Check::Searching => "busy",
        _ if matches!(state.check, Check::Failed(_)) => "error",
        _ if state.offer.is_some() => "available",
        _ if state.check == Check::Current => "success",
        _ if state.check == Check::Local => "local",
        _ => "available",
    };
    let checked = state
        .checked
        .map(chrono::DateTime::<chrono::Local>::from)
        .map(|time| time.format("%Y-%m-%d · %H:%M").to_string());
    let (got, total) = state.progress;
    let pct = total
        .filter(|total| *total > 0)
        .map(|total| got.min(total) * 100 / total);
    rsx! {
        style { {include_str!("../assets/updates.css")} }
        section { class: "dxf-updates",
            h3 { {crate::i18n::text("Updates", &[])} }
            div { class: "dxf-update-card",
                div { class: "dxf-update-card-heading", role: "status", "aria-live": "polite",
                    UpdateStatusIcon { kind }
                    div { h4 { {crate::i18n::label(title)} } p { {crate::i18n::label(detail)} } }
                }
                div { class: "dxf-update-versions",
                    span { {crate::i18n::text("Installed version: {0}", &[crate::version::VERSION.to_string()])} }
                    if let Some(offer) = &state.offer { span { {crate::i18n::text("Available version: {0}", std::slice::from_ref(&offer.tag))} } }
                }
                if state.phase == Phase::Downloading {
                    if let Some(pct) = pct {
                        progress { max: "100", value: "{pct}", aria_label: crate::i18n::text("Update download progress", &[]) }
                        p { "{pct}% · {super::mib(got)} / {super::mib(total.unwrap_or(0))}" }
                    } else {
                        progress { aria_label: crate::i18n::text("Update download progress", &[]) }
                        p { {crate::i18n::text("{0} downloaded", &[super::mib(got)])} }
                    }
                }
                if let Phase::Failed(message) = &state.phase { p { class: "dxf-update-error", role: "alert", {crate::i18n::message(message)} } }
                if state.offer.is_some() {
                  div { class: "dxf-update-actions",
                    if let Phase::Ready(_) = &state.phase {
                        button { class: "dxf-update-primary", disabled: state.in_call, onclick: move |_| updates.request_install(), {crate::i18n::text("Restart and update", &[])} }
                    } else if matches!(state.phase, Phase::Offered | Phase::Failed(_)) && state.offer.as_ref().is_some_and(|offer| offer.download.is_some()) {
                        button { class: "dxf-update-primary", disabled: state.check == Check::Searching, onclick: move |_| updates.download(), {crate::i18n::text("Download update", &[])} }
                    }
                    if let Some(offer) = state.offer.clone() {
                        button { class: "dxf-update-secondary", onclick: move |_| crate::app::open_external(&offer.url), {crate::i18n::text("View release notes", &[])} }
                    }
                  }
                }
                if matches!(state.phase, Phase::Ready(_)) && state.in_call { p { {crate::i18n::text("Finish your call before restarting. The download is ready.", &[])} } }
                if let Check::Failed(message) = &state.check {
                    p { class: "dxf-update-error", role: "alert", {crate::i18n::text("Could not check for updates. Try again.", &[])} }
                    details { summary { {crate::i18n::text("Details", &[])} } {crate::i18n::message(message)} }
                }
                div { class: "dxf-update-footer",
                    if let Some(checked) = checked {
                        p { {crate::i18n::text("Last checked: {0}", &[checked])} }
                    } else {
                        p { {crate::i18n::text("No checks yet", &[])} }
                    }
                    button { class: "dxf-update-secondary", disabled: !state.can_check(), onclick: move |_| updates.check(),
                        if state.check == Check::Searching { {crate::i18n::text("Checking for updates…", &[])} }
                        else { {crate::i18n::text("Check for updates", &[])} }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn offer(tag: &str) -> Update {
        Update {
            tag: tag.into(),
            url: "https://github.com/ssotomayor/discordia/releases".into(),
            download: Some(crate::version::Download {
                asset: "test".into(),
                asset_url: "https://example.invalid/test".into(),
                signature_url: "https://example.invalid/test.minisig".into(),
            }),
        }
    }

    #[test]
    fn a_failed_check_keeps_the_offer_and_a_new_release_restores_the_banner() {
        let mut state = State::default();
        state.checked(Ok(crate::version::CheckResult::Available(offer("v1"))));
        state.dismissed = true;
        state.checked(Err("offline".into()));
        assert_eq!(state.offer, Some(offer("v1")));
        assert!(state.dismissed);
        assert!(state.checked.is_some());
        assert!(state.can_check());
        state.checked(Ok(crate::version::CheckResult::Available(offer("v1"))));
        assert!(state.dismissed);
        state.checked(Ok(crate::version::CheckResult::Available(offer("v2"))));
        assert!(!state.dismissed);
        assert_eq!(state.offer, Some(offer("v2")));
    }

    #[test]
    fn checks_distinguish_local_current_and_failure_and_wait_for_downloads() {
        let mut state = State::default();
        state.checked(Ok(crate::version::CheckResult::Local));
        assert_eq!(state.check, Check::Local);
        state.checked(Ok(crate::version::CheckResult::Current));
        assert_eq!(state.check, Check::Current);
        state.checked(Err("offline".into()));
        assert_eq!(state.check, Check::Failed("offline".into()));
        for phase in [
            Phase::Downloading,
            Phase::Ready("verified".into()),
            Phase::Leaving("verified".into()),
            Phase::Installing,
            Phase::Finished,
        ] {
            state.phase = phase;
            assert!(!state.can_check());
        }
        state.phase = Phase::Offered;
        state.check = Check::Searching;
        assert!(!state.can_check());
    }

    type Export = Rc<RefCell<Option<Updates>>>;

    #[test]
    fn downloads_do_not_install_and_restart_waits_for_calls_and_disconnect() {
        let export: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| State {
                    offer: Some(offer("v1")),
                    ..Default::default()
                });
                let open_request = use_signal(|| 0);
                let commands = use_coroutine(|_rx: UnboundedReceiver<Command>| async {});
                *export.borrow_mut() = Some(Updates {
                    state,
                    open_request,
                    commands,
                });
                rsx! { div {} }
            },
            export.clone(),
        );
        dom.rebuild_in_place();
        let mut updates = export.borrow().unwrap();
        updates.check();
        updates.download();
        assert_eq!(updates.state.peek().phase, Phase::Offered);
        updates.state.write().check = Check::Idle;
        updates.download();
        assert_eq!(updates.state.peek().phase, Phase::Downloading);
        updates.request_install();
        assert_eq!(updates.state.peek().phase, Phase::Downloading);
        let path = PathBuf::from("verified");
        updates.state.write().phase = Phase::Ready(path.clone());
        updates.state.write().in_call = true;
        updates.request_install();
        assert_eq!(updates.state.peek().phase, Phase::Ready(path.clone()));
        updates.state.write().in_call = false;
        updates.request_install();
        assert_eq!(updates.state.peek().phase, Phase::Leaving(path.clone()));
        assert!(updates.prevents_close());
        updates.state.write().in_call = true;
        updates.install_after_disconnect();
        assert_eq!(updates.state.peek().phase, Phase::Leaving(path));
        updates.state.write().in_call = false;
        updates.install_after_disconnect();
        assert_eq!(updates.state.peek().phase, Phase::Installing);
        assert!(updates.prevents_close());
        updates.state.write().phase = Phase::Failed("installer unavailable".into());
        assert!(!updates.prevents_close());
        assert!(updates.state.peek().can_check());
    }
}
