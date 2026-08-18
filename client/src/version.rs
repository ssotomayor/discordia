//! Which build this is.
//!
//! Stamped by `build.rs` at compile time — see there for why it is not
//! `CARGO_PKG_VERSION`, which is `0.1.0` in every release this project has ever
//! published.
//!
//! Two shapes, and telling them apart matters more than either one:
//!
//! - `v0.1.0-pre.223` — a CI build, and the exact tag of the GitHub release it
//!   was published under. Paste it into a report and the artifact is findable.
//! - `0.1.0-dev+a1b2c3d` — anything else. Nobody downloaded this.

use std::time::Duration;

use dioxus::prelude::*;

/// The version string this binary was built with.
pub const VERSION: &str = env!("DISCORDIA_VERSION");

/// How long to wait for GitHub before giving up.
///
/// A blackholed network — captive portal, a firewall that accepts the handshake
/// and then says nothing — would otherwise leave this one-shot task pending for
/// the life of the app. Nothing awaits it, so that is invisible rather than
/// harmful, but "silent on every failure" should mean the failure *happens*.
/// Shorter than `blossom`'s 30s because nobody is waiting on the answer.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The releases endpoint for whichever repository this build came from.
///
/// Derived from the crate's `repository` field rather than hardcoded, because
/// hardcoding it is only correct for one build. A fork shipping its own
/// binaries would have inherited an update check against **this** repo — no
/// error, no log, just a button pointing users at unrelated releases, or one
/// that never appears because upstream is behind them.
///
/// Anything that is not a `https://github.com/owner/name` URL disables the
/// check. Failing closed is the point: a fork on GitLab should get no notice
/// rather than someone else's.
///
/// Parsed with `url` rather than by hand. The hand-rolled version — strip the
/// prefix, count the slashes — accepted `https://github.com//name` as a repo
/// whose owner is the empty string, and built `…/repos//name/releases` out of
/// it. A parser that understands path segments cannot make that mistake,
/// and the crate was already a dependency.
fn releases_url_from(repository: &str) -> Option<String> {
    let url = url::Url::parse(repository.trim()).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return None;
    }
    // Empty segments dropped so a trailing slash — common in a `repository`
    // field — reads the same as none.
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());
    let owner = segments.next()?;
    let name = segments.next()?.trim_end_matches(".git");
    // A third segment means a path *into* the repo (`/tree/master`), not the
    // repo. `name` can still be empty after stripping a bare `.git`.
    if segments.next().is_some() || name.is_empty() {
        return None;
    }
    Some(format!(
        "https://api.github.com/repos/{owner}/{name}/releases"
    ))
}

fn releases_url() -> Option<String> {
    releases_url_from(env!("CARGO_PKG_REPOSITORY"))
}

/// A published release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The tag, which is also what the app shows for its own version.
    pub tag: String,
    /// The release page, opened in the user's real browser.
    pub url: String,
}

/// One row of the releases listing, cut down to what we use.
#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
}

/// The build number inside a release tag: `v0.1.0-pre.223` → `223`.
///
/// Returns `None` for anything else, which is what makes a dev build and a
/// hand-made tag both simply not compare rather than compare wrongly.
///
/// **Parsed to a number on purpose.** CI names releases after
/// `github.run_number`, so the tags sort numerically and *not* lexically —
/// `"v0.1.0-pre.99" > "v0.1.0-pre.223"` as strings, which would tell everyone
/// on the newest build that they are behind.
fn release_number(tag: &str) -> Option<u64> {
    tag.rsplit_once("-pre.")?.1.parse().ok()
}

/// The newest release in a listing that is newer than `mine`.
///
/// Split out from the request so the choosing can be tested without a network.
///
/// `/releases/latest` would have been the obvious endpoint and is the wrong
/// one: it excludes prereleases, and every release this project publishes is
/// one (`prerelease: true`, `make_latest: "false"` in `ci.yml`). It would
/// answer 404 forever.
fn newest_release(releases: Vec<Release>, mine: u64) -> Option<Update> {
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| release_number(&r.tag_name).map(|n| (n, r)))
        .filter(|(n, _)| *n > mine)
        .max_by_key(|(n, _)| *n)
        .map(|(_, r)| Update {
            tag: r.tag_name,
            url: r.html_url,
        })
}

/// Ask GitHub whether anything newer has been published.
///
/// **Notifies only.** Nothing is downloaded and nothing is executed, which is
/// what keeps this out of the signing question entirely: the app has no
/// Authenticode certificate, so a self-updater would be handing the user an
/// unsigned binary it could not verify. A link the user follows deliberately is
/// a different act from a binary the app swaps underneath them.
///
/// Silent **to the user** on every failure — no network, a rate limit (60/hour
/// unauthenticated, per IP), a shape we do not recognise. Being unable to check
/// is not news; the user came here to use the app.
///
/// Not silent to a developer, though, and that distinction is the whole reason
/// for the `eprintln!`s. Without them a malformed URL, a TLS failure and
/// "GitHub changed the response shape" all collapse into the same invisible
/// `None` as "you are up to date" — indistinguishable from a working check, so
/// a broken one could ship unnoticed for months. Same `[tag] message` shape the
/// rest of the client uses.
pub async fn check_for_update() -> Option<Update> {
    // Dev builds have no release to compare against; checking would be noise.
    let mine = is_release().then(|| release_number(VERSION))??;
    let url = releases_url().or_else(|| {
        eprintln!(
            "[update] no update check: {:?} is not a github.com/owner/name repository",
            env!("CARGO_PKG_REPOSITORY")
        );
        None
    })?;

    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .inspect_err(|e| eprintln!("[update] could not build the http client: {e}"))
        .ok()?;
    let res = client
        .get(&url)
        // GitHub rejects API requests with no User-Agent outright.
        .header("User-Agent", format!("Discordia/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .inspect_err(|e| eprintln!("[update] request to {url} failed: {e}"))
        .ok()?;
    if !res.status().is_success() {
        // The one worth reading twice: 403 here is the unauthenticated rate
        // limit, not a permissions problem.
        eprintln!("[update] {url} answered {}", res.status());
        return None;
    }
    let releases = res
        .json()
        .await
        .inspect_err(|e| eprintln!("[update] could not read the releases listing: {e}"))
        .ok()?;
    newest_release(releases, mine)
}

/// Whether this build came from CI and corresponds to a published release.
///
/// The test is the `-dev` marker `build.rs` puts on everything else, rather
/// than a pattern match on the release shape: the fallback is ours to define
/// and the tag format is not — it has already been `v0.1.0-pre.N` for 223
/// releases, but nothing here should break the day it stops being.
pub fn is_release() -> bool {
    !VERSION.contains("-dev")
}

/// The build string, styled but **not positioned** — the shell places it.
///
/// Shown on the connect and identity screens only, and deliberately not in the
/// workspace: once you are in, the version is chrome you would read past
/// forever to answer a question asked roughly never. Before connecting is where
/// it is looked for, and it is the only screen reachable when nothing works —
/// which is exactly when someone has to say what they are running.
///
/// Selectable on purpose: its whole job is to be pasted into a report.
#[component]
pub fn VersionLabel() -> Element {
    rsx! {
        span {
            class: "text-[10px] text-[var(--text-dim)]",
            title: if is_release() {
                "This build's release tag on GitHub"
            } else {
                "A local build — no release was published for it"
            },
            "{VERSION}"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stamp exists and is not the crate version bare, which is what it
    /// would silently fall back to if the build script stopped running.
    #[test]
    fn the_build_is_stamped() {
        assert!(!VERSION.is_empty());
        assert_ne!(
            VERSION,
            env!("CARGO_PKG_VERSION"),
            "an unstamped build is indistinguishable from every other release"
        );
    }

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("https://example.invalid/{tag}"),
            draft: false,
        }
    }

    /// A fork gets its own releases, or none — never ours.
    #[test]
    fn the_endpoint_follows_the_repository_field() {
        let ours = "https://api.github.com/repos/ssotomayor/discordia/releases";
        assert_eq!(
            releases_url_from("https://github.com/ssotomayor/discordia").as_deref(),
            Some(ours)
        );
        for variant in [
            "https://github.com/ssotomayor/discordia/",
            "https://github.com/ssotomayor/discordia.git",
            "  https://github.com/ssotomayor/discordia  ",
        ] {
            assert_eq!(
                releases_url_from(variant).as_deref(),
                Some(ours),
                "did not normalise {variant:?}"
            );
        }
        assert_eq!(
            releases_url_from("https://github.com/someone/fork").as_deref(),
            Some("https://api.github.com/repos/someone/fork/releases")
        );
    }

    /// Anything we cannot read as `github.com/owner/name` disables the check.
    /// Failing closed is the point — a fork elsewhere should get no notice
    /// rather than somebody else's.
    #[test]
    fn an_unrecognised_repository_disables_the_check() {
        for bad in [
            "",
            "https://gitlab.com/someone/thing",
            "https://github.com/owner",
            "https://github.com/owner/name/tree/master",
            "git@github.com:owner/name.git",
            "http://github.com/owner/name",
            "https://github.com//name",
            "https://github.com//",
            "https://github.com/owner/.git",
        ] {
            assert_eq!(releases_url_from(bad), None, "accepted {bad:?}");
        }
    }

    /// And that this build's own field is one we accept — a typo there would
    /// silently disable the feature everywhere.
    #[test]
    fn this_build_has_a_usable_repository() {
        assert!(
            releases_url().is_some(),
            "CARGO_PKG_REPOSITORY is {:?}",
            env!("CARGO_PKG_REPOSITORY")
        );
    }

    /// The trap this whole comparison exists to avoid. CI names releases after
    /// `github.run_number`, so tags order numerically and not lexically — as
    /// strings `"v0.1.0-pre.99" > "v0.1.0-pre.223"`, which would tell everyone
    /// on the newest build that they were behind.
    #[test]
    fn releases_are_ordered_by_number_not_by_string() {
        assert!("v0.1.0-pre.99" > "v0.1.0-pre.223", "premise of this test");

        let found = newest_release(
            vec![release("v0.1.0-pre.99"), release("v0.1.0-pre.223")],
            98,
        );
        assert_eq!(found.map(|u| u.tag).as_deref(), Some("v0.1.0-pre.223"));
    }

    /// Nothing newer means no notice. The listing always contains our own
    /// release and every one before it, so "greater than mine" is the whole
    /// filter — without it the newest build would still be told to update.
    #[test]
    fn being_current_reports_nothing() {
        let listing = || vec![release("v0.1.0-pre.222"), release("v0.1.0-pre.223")];
        assert_eq!(newest_release(listing(), 223), None);
        assert_eq!(newest_release(listing(), 224), None, "ahead of the listing");
        assert!(newest_release(listing(), 222).is_some());
    }

    /// Drafts are visible to anyone who can read the repo and are not published
    /// artifacts — pointing a user at one offers a download that is not there.
    #[test]
    fn drafts_are_not_offered() {
        let mut draft = release("v0.1.0-pre.900");
        draft.draft = true;
        assert_eq!(newest_release(vec![draft], 1), None);
    }

    /// A tag we cannot parse does not compare, rather than comparing wrongly.
    #[test]
    fn an_unparseable_tag_is_ignored() {
        assert_eq!(release_number("v0.2.0"), None);
        assert_eq!(release_number("0.1.0-dev+a1b2c3d"), None);
        assert_eq!(release_number("v0.1.0-pre.x"), None);
        assert_eq!(release_number("v0.1.0-pre.223"), Some(223));
        assert_eq!(newest_release(vec![release("v1.2.3")], 1), None);
    }

    /// Tests run from a checkout, never from a published artifact — so the
    /// binary under test must not claim to be a release. If this ever fails in
    /// CI it means a check job inherited a release stamp it should not have.
    #[test]
    fn a_test_build_is_not_a_release() {
        assert!(
            VERSION.contains("-dev"),
            "built as {VERSION}, which claims to be a published release"
        );
        assert!(!is_release());
    }
}
