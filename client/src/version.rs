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

/// Whether this build came from CI and corresponds to a published release.
///
/// The test is the `-dev` marker `build.rs` puts on everything else, rather
/// than a pattern match on the release shape: the fallback is ours to define
/// and the tag format is not — it has already been `v0.1.0-pre.N` for 223
/// releases, but nothing here should break the day it stops being.
pub fn is_release() -> bool {
    !VERSION.contains("-dev")
}

/// The build string, styled but **not positioned** — the caller decides where
/// it sits, because the two places it appears want different things.
///
/// On the connect and identity screens it is a fixed badge in an otherwise
/// empty corner. In the workspace it is one item inside the layout-controls
/// cluster, whose `gap` already spaces it; a second fixed corner there would
/// land on top of that cluster or on the activity launcher, which hold the only
/// two corners the workspace has.
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
