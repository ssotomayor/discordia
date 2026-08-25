use std::time::Duration;

use dioxus::prelude::*;

pub const VERSION: &str = env!("DISCORDIA_VERSION");

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

fn releases_url_from(repository: &str) -> Option<String> {
    let url = url::Url::parse(repository.trim()).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return None;
    }
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());
    let owner = segments.next()?;
    let name = segments.next()?.trim_end_matches(".git");
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub tag: String,
    pub url: String,
    pub download: Option<Download>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub asset: String,
    pub asset_url: String,
    pub signature_url: String,
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn download_for(assets: &[Asset]) -> Option<Download> {
    let wanted = crate::update::asset_name()?;
    let sig = crate::update::signature_name(wanted);
    let find = |name: &str| {
        assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    Some(Download {
        asset: wanted.to_string(),
        asset_url: find(wanted)?,
        signature_url: find(&sig)?,
    })
}

fn release_number(tag: &str) -> Option<u64> {
    tag.rsplit_once("-pre.")?.1.parse().ok()
}

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
            download: download_for(&r.assets),
        })
}

pub async fn check_for_update() -> Option<Update> {
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
        .header("User-Agent", format!("Discordia/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .inspect_err(|e| eprintln!("[update] request to {url} failed: {e}"))
        .ok()?;
    if !res.status().is_success() {
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

pub fn is_release() -> bool {
    !VERSION.contains("-dev")
}

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
            assets: vec![],
        }
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            browser_download_url: format!("https://example.invalid/{name}"),
        }
    }

    #[test]
    fn an_artifact_without_its_signature_is_not_offered() {
        let Some(name) = crate::update::asset_name() else {
            return;
        };
        assert_eq!(download_for(&[asset(name)]), None);
        let both = [asset(name), asset(&crate::update::signature_name(name))];
        let d = download_for(&both).expect("both halves present");
        assert_eq!(d.asset, name);
        assert!(d.signature_url.ends_with(".minisig"));
    }

    #[test]
    fn another_platforms_artifact_is_not_mistaken_for_ours() {
        let foreign = [
            asset("Discordia-solaris-sparc.tar.gz"),
            asset("Discordia-solaris-sparc.tar.gz.minisig"),
        ];
        assert_eq!(download_for(&foreign), None);
    }

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

    #[test]
    fn this_build_has_a_usable_repository() {
        assert!(
            releases_url().is_some(),
            "CARGO_PKG_REPOSITORY is {:?}",
            env!("CARGO_PKG_REPOSITORY")
        );
    }

    #[test]
    fn releases_are_ordered_by_number_not_by_string() {
        assert!("v0.1.0-pre.99" > "v0.1.0-pre.223", "premise of this test");

        let found = newest_release(
            vec![release("v0.1.0-pre.99"), release("v0.1.0-pre.223")],
            98,
        );
        assert_eq!(found.map(|u| u.tag).as_deref(), Some("v0.1.0-pre.223"));
    }

    #[test]
    fn being_current_reports_nothing() {
        let listing = || vec![release("v0.1.0-pre.222"), release("v0.1.0-pre.223")];
        assert_eq!(newest_release(listing(), 223), None);
        assert_eq!(newest_release(listing(), 224), None, "ahead of the listing");
        assert!(newest_release(listing(), 222).is_some());
    }

    #[test]
    fn drafts_are_not_offered() {
        let mut draft = release("v0.1.0-pre.900");
        draft.draft = true;
        assert_eq!(newest_release(vec![draft], 1), None);
    }

    #[test]
    fn an_unparseable_tag_is_ignored() {
        assert_eq!(release_number("v0.2.0"), None);
        assert_eq!(release_number("0.1.0-dev+a1b2c3d"), None);
        assert_eq!(release_number("v0.1.0-pre.x"), None);
        assert_eq!(release_number("v0.1.0-pre.223"), Some(223));
        assert_eq!(newest_release(vec![release("v1.2.3")], 1), None);
    }

    #[test]
    fn a_test_build_is_not_a_release() {
        assert!(
            VERSION.contains("-dev"),
            "built as {VERSION}, which claims to be a published release"
        );
        assert!(!is_release());
    }
}
