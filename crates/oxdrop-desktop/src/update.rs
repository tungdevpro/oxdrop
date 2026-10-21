//! The "new version available" notice: one anonymous look at the latest GitHub release.

use std::time::Duration;

/// Looks up this repository's latest release once, anonymously, and returns it if it
/// is newer than the running app. Any failure, including the repository being private,
/// is silent. This is the only request Oxdrop makes beyond the local network.
pub async fn check() -> Option<Release> {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let running = env!("CARGO_PKG_VERSION");
    let url = latest_release_api(repository)?;
    let client = reqwest::Client::builder()
        // GitHub's API refuses requests without one.
        .user_agent(concat!("Oxdrop/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::info!(status = %response.status(), "no release to compare with");
        return None;
    }
    let release = newer_release(&response.text().await.ok()?, running, repository);
    if let Some(release) = &release {
        tracing::info!(tag = release.tag, "a newer release is available");
    }
    release
}

/// The GitHub API address of the latest release of `repository`, such as
/// `https://github.com/owner/name`.
fn latest_release_api(repository: &str) -> Option<String> {
    let path = repository
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")?;
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("https://api.github.com/repos/{owner}/{name}/releases/latest"))
}

/// A release newer than the running app, to tell the user about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its tag, such as `v1.2.0`.
    pub tag: String,
    /// Its page on GitHub, where it can be downloaded.
    pub url: String,
}

/// The release to tell the user about, given the body of GitHub's "latest release"
/// response for `repository` (such as `https://github.com/owner/name`): only one
/// that is newer than `running` and whose page really is one of that repository's.
pub fn newer_release(body: &str, running: &str, repository: &str) -> Option<Release> {
    #[derive(serde::Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
    }
    let latest: Latest = serde_json::from_str(body).ok()?;
    let ours = latest
        .html_url
        .starts_with(&format!("{}/releases/", repository.trim_end_matches('/')));
    (ours && is_newer(&latest.tag_name, running)).then_some(Release {
        tag: latest.tag_name,
        url: latest.html_url,
    })
}

/// Whether the release tagged `tag` (such as `v1.2.0`) is newer than `running`.
/// A tag that isn't a version never is.
pub fn is_newer(tag: &str, running: &str) -> bool {
    let parse =
        |version: &str| semver::Version::parse(version.strip_prefix('v').unwrap_or(version));
    match (parse(tag.trim()), parse(running)) {
        (Ok(tag), Ok(running)) => tag > running,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_version_is_newer_compared_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("v0.9.0", "0.10.0"));
    }

    #[test]
    fn a_pre_release_ranks_below_its_release() {
        assert!(!is_newer("v0.2.0-beta.1", "0.2.0"));
        assert!(is_newer("v0.2.0", "0.2.0-beta.1"));
        assert!(is_newer("v0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(is_newer("v0.3.0-rc.1", "0.2.0"));
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_never_newer() {
        for tag in [
            "",
            "latest",
            "v",
            "v1",
            "v1.2",
            "release-2026",
            "vv1.0.0",
            "1.0.0.0",
        ] {
            assert!(!is_newer(tag, "0.1.0"), "{tag:?}");
        }
    }

    const REPOSITORY: &str = "https://github.com/tungdevpro/oxdrop";

    fn latest(tag: &str, url: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "html_url": url,
            "name": "Oxdrop",
            "draft": false,
            "prerelease": false,
            "assets": [],
        })
        .to_string()
    }

    #[test]
    fn a_newer_release_is_offered_with_its_page() {
        let url = "https://github.com/tungdevpro/oxdrop/releases/tag/v0.2.0";

        assert_eq!(
            newer_release(&latest("v0.2.0", url), "0.1.0", REPOSITORY),
            Some(Release {
                tag: "v0.2.0".to_owned(),
                url: url.to_owned(),
            })
        );
        assert_eq!(
            newer_release(&latest("v0.2.0", url), "0.2.0", REPOSITORY),
            None
        );
    }

    #[test]
    fn a_page_outside_the_repository_is_never_offered() {
        for url in [
            "https://evil.example/tungdevpro/oxdrop/releases/tag/v9.0.0",
            "https://github.com/tungdevpro/oxdrop-fork/releases/tag/v9.0.0",
            "http://github.com/tungdevpro/oxdrop/releases/tag/v9.0.0",
            "https://github.com/tungdevpro/oxdrop/issues/1",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                newer_release(&latest("v9.0.0", url), "0.1.0", REPOSITORY),
                None,
                "{url}"
            );
        }
    }

    #[test]
    fn anything_unexpected_is_silently_ignored() {
        for body in [
            "",
            "not json",
            r#"{"message": "Not Found", "documentation_url": "https://docs.github.com"}"#,
            r#"{"tag_name": 2, "html_url": "https://github.com/tungdevpro/oxdrop/releases/tag/v2"}"#,
        ] {
            assert_eq!(newer_release(body, "0.1.0", REPOSITORY), None, "{body:?}");
        }
    }

    #[test]
    fn the_latest_release_is_looked_up_for_this_repository() {
        assert_eq!(
            latest_release_api(REPOSITORY).as_deref(),
            Some("https://api.github.com/repos/tungdevpro/oxdrop/releases/latest")
        );
        assert_eq!(latest_release_api("https://gitlab.com/a/b"), None);
        assert_eq!(latest_release_api("https://github.com/a"), None);
    }
}
