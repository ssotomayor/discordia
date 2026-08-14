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

use dioxus::prelude::*;

/// The version string this binary was built with.
pub const VERSION: &str = env!("DISCORDIA_VERSION");

/// Where releases are published. Listed, not `/releases/latest` — see
/// `newest_release`.
const RELEASES_URL: &str = "https://api.github.com/repos/ssotomayor/discordia/releases";

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
/// Silent on every failure — no network, a rate limit (60/hour unauthenticated,
/// per IP), a shape we do not recognise. Being unable to check is not news; the
/// user came here to use the app.
pub async fn check_for_update() -> Option<Update> {
    // A dev build has nothing to compare against, and telling someone who
    // compiled the tree that a release exists is noise rather than news.
    let mine = is_release().then(|| release_number(VERSION))??;

    let res = reqwest::Client::new()
        .get(RELEASES_URL)
        // GitHub rejects API requests with no User-Agent outright.
        .header("User-Agent", format!("Discordia/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    newest_release(res.json().await.ok()?, mine)
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
