//! New versions of Pulse, from its releases on GitHub.
//!
//! This reads GitHub's answer about the latest release: whether it's newer, and which of its
//! files this copy installs from. The platform does the asking, downloading and installing,
//! through [`UpdateHost`]. The app decides when.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Asked for the latest release, and nothing about the user.
pub const LATEST_RELEASE: &str = "https://api.github.com/repos/Beroris/pulse/releases/latest";
/// The only place a release's files may come from.
pub const DOWNLOADS_FROM: &str = "https://github.com/";
/// The releases page, for a copy that can't install a new version itself.
pub const RELEASES_PAGE: &str = "https://github.com/Beroris/pulse/releases/latest";

/// How this copy of Pulse takes a new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Installed by Setup: runs the new Setup.
    Installed,
    /// Named "…Portable…": swaps its own exe for the new one.
    Portable,
    /// Anything else, such as a build from source: links to the release page.
    Other,
}

impl Kind {
    pub fn installs_itself(self) -> bool {
        self != Kind::Other
    }

    /// The end of the release file's name this kind of copy installs from.
    fn asset_suffix(self) -> Option<&'static str> {
        match self {
            Kind::Installed => Some("-Setup.exe"),
            Kind::Portable => Some("-Portable.exe"),
            Kind::Other => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Such as "1.1.0".
    pub version: String,
    /// The release's page, with its notes.
    pub page: String,
    /// The file to install from, when this kind of copy installs itself.
    pub download: Option<Download>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub name: String,
    pub url: String,
    /// In bytes; 0 when GitHub didn't say.
    pub size: u64,
    /// Lowercase hex, when GitHub lists one.
    pub sha256: Option<String>,
}

/// Where an update has got to.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    /// A newer version is coming down.
    Downloading(String),
    /// Downloaded and checked: one click installs it.
    Ready(String),
    /// Newer, but this copy can't install it: the release page has it.
    Available(String),
    Installing,
    Failed(String),
}

/// What a platform does for updates. Everything but `kind` blocks, so the app calls it on a
/// worker thread.
pub trait UpdateHost: Send + Sync {
    fn kind(&self) -> Kind;
    /// Asks GitHub for the latest release. `Ok(None)` when this copy is up to date.
    fn check(&self) -> Result<Option<Release>, String>;
    /// Downloads the new version, checks it against what GitHub lists, and puts it where this
    /// kind of copy installs it from.
    fn download(&self, d: &Download) -> Result<PathBuf, String>;
    /// Starts installing the downloaded version. The app quits straight after, so the new
    /// version can take its place, and the new version starts once it's in.
    fn install(&self, file: &Path) -> Result<(), String>;
    /// Clears what an earlier update left behind.
    fn clean_up(&self);
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// Such as "sha256:0123…".
    #[serde(default)]
    digest: Option<String>,
}

/// "1.2.3" or "v1.2.3". Anything else, like "1.2.3-beta", isn't a release to offer.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let mut parts = text.split('.');
    let mut number = || -> Option<u64> {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let version = (number()?, number()?, number()?);
    parts.next().is_none().then_some(version)
}

/// Reads GitHub's answer for the latest release. `Ok(None)` when it isn't newer than `current`.
pub fn parse_release(json: &[u8], kind: Kind, current: &str) -> Result<Option<Release>, String> {
    let release: ReleaseJson = serde_json::from_slice(json).map_err(|_| "GitHub's answer didn't make sense.".to_string())?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let (Some(version), Some(current)) = (parse_version(&release.tag_name), parse_version(current)) else {
        return Ok(None);
    };
    if version <= current {
        return Ok(None);
    }
    let download = kind.asset_suffix().and_then(|suffix| {
        release
            .assets
            .into_iter()
            .find(|a| a.name.ends_with(suffix) && is_plain_file_name(&a.name) && a.browser_download_url.starts_with(DOWNLOADS_FROM))
            .map(|a| Download {
                sha256: a
                    .digest
                    .as_deref()
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase),
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            })
    });
    let page = if release.html_url.starts_with(DOWNLOADS_FROM) { release.html_url } else { RELEASES_PAGE.to_string() };
    Ok(Some(Release { version: format!("{}.{}.{}", version.0, version.1, version.2), page, download }))
}

/// A file name with no folder in it, so it can't be saved anywhere but where it's meant to go.
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != ".." && !name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(tag: &str, extra: &str) -> Vec<u8> {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/Beroris/pulse/releases/tag/{tag}",{extra}"assets":[
                {{"name":"Pulse-1.1.0-Setup.exe","browser_download_url":"https://github.com/Beroris/pulse/releases/download/v1.1.0/Pulse-1.1.0-Setup.exe","size":900000,"digest":"sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}},
                {{"name":"Pulse-1.1.0-Portable.exe","browser_download_url":"https://github.com/Beroris/pulse/releases/download/v1.1.0/Pulse-1.1.0-Portable.exe","size":1500000}}
            ]}}"#
        )
        .into_bytes()
    }

    #[test]
    fn versions_are_plain_semver() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v10.0.1"), Some((10, 0, 1)));
        assert_eq!(parse_version(" V1.0.0 "), Some((1, 0, 0)));
        for bad in ["1.2", "1.2.3.4", "1.2.3-beta", "1..3", "", "v", "1.2.x", "+1.2.3"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
        assert!(parse_version("1.10.0") > parse_version("1.9.9"));
    }

    #[test]
    fn only_a_newer_release_is_offered() {
        assert_eq!(parse_release(&answer("v1.1.0", ""), Kind::Installed, "1.1.0"), Ok(None));
        assert_eq!(parse_release(&answer("v1.0.9", ""), Kind::Installed, "1.1.0"), Ok(None));
        assert_eq!(parse_release(&answer("v1.1.0", r#""prerelease":true,"#), Kind::Installed, "1.0.0"), Ok(None));
        assert_eq!(parse_release(&answer("v1.1.0", r#""draft":true,"#), Kind::Installed, "1.0.0"), Ok(None));
        assert_eq!(parse_release(&answer("v1.1.0-rc1", ""), Kind::Installed, "1.0.0"), Ok(None));
        assert!(parse_release(b"not json", Kind::Installed, "1.0.0").is_err());
    }

    #[test]
    fn each_copy_gets_its_own_file() {
        let setup = parse_release(&answer("v1.1.0", ""), Kind::Installed, "1.0.0").unwrap().unwrap();
        assert_eq!(setup.version, "1.1.0");
        let d = setup.download.unwrap();
        assert_eq!(d.name, "Pulse-1.1.0-Setup.exe");
        assert_eq!(d.size, 900000);
        assert_eq!(d.sha256.as_deref(), Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"));
        let portable = parse_release(&answer("v1.1.0", ""), Kind::Portable, "1.0.0").unwrap().unwrap();
        assert_eq!(portable.download.unwrap().name, "Pulse-1.1.0-Portable.exe");
        let other = parse_release(&answer("v1.1.0", ""), Kind::Other, "1.0.0").unwrap().unwrap();
        assert_eq!(other.download, None);
        assert_eq!(other.page, "https://github.com/Beroris/pulse/releases/tag/v1.1.0");
    }

    #[test]
    fn files_from_elsewhere_are_refused() {
        let json = br#"{"tag_name":"v2.0.0","html_url":"https://evil.example/","assets":[
            {"name":"Pulse-2.0.0-Setup.exe","browser_download_url":"https://evil.example/Pulse-2.0.0-Setup.exe"},
            {"name":"..\\Pulse-2.0.0-Portable.exe","browser_download_url":"https://github.com/x"}]}"#;
        let r = parse_release(json, Kind::Installed, "1.0.0").unwrap().unwrap();
        assert_eq!(r.download, None);
        assert_eq!(r.page, RELEASES_PAGE);
        assert_eq!(parse_release(json, Kind::Portable, "1.0.0").unwrap().unwrap().download, None);
        assert!(!is_plain_file_name("a/b.exe") && !is_plain_file_name("C:x.exe") && !is_plain_file_name(".."));
        assert!(is_plain_file_name("Pulse-1.1.0-Setup.exe"));
    }
}
