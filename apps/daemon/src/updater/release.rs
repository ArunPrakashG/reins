use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum UpdaterError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid release payload: {0}")]
    InvalidPayload(String),
    #[error("no release asset found for this platform ('{0}')")]
    NoMatchingAsset(String),
    #[error("checksum mismatch for downloaded asset")]
    ChecksumMismatch,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

pub fn parse_release_json(body: &str) -> Result<ReleaseInfo, UpdaterError> {
    serde_json::from_str(body).map_err(|e| UpdaterError::InvalidPayload(e.to_string()))
}

/// The asset filename this platform's `reins update` should look for, matching the
/// naming convention `scripts/build-release.sh` produces (`reins-<os>-<arch>.tar.gz`).
pub fn platform_asset_name() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "reins-linux-x86_64.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "reins-linux-aarch64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "reins-macos-x86_64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "reins-macos-aarch64.tar.gz"
    }
}

pub fn pick_asset<'a>(release: &'a ReleaseInfo, asset_name: &str) -> Option<&'a ReleaseAsset> {
    release.assets.iter().find(|a| a.name == asset_name)
}

/// Compares a bare crate version (`"0.1.2"`, no leading `v`) against a GitHub release
/// tag (`"v0.2.0"` or `"0.2.0"`) by parsing each into a `(major, minor, patch)` tuple.
/// A tag that fails to parse is treated as not-newer (fail closed: never nag the user
/// about a malformed tag).
pub fn version_is_newer(current: &str, latest_tag: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(current), parse(latest_tag)) {
        (Some(cur), Some(latest)) => latest > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_json_reads_tag_and_assets() {
        let body = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "reins-linux-x86_64.tar.gz", "browser_download_url": "https://example.com/a.tar.gz"},
                {"name": "SHA256SUMS", "browser_download_url": "https://example.com/sums.txt"}
            ]
        }"#;
        let release = parse_release_json(body).unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
    }

    #[test]
    fn parse_release_json_rejects_garbage() {
        assert!(parse_release_json("not json").is_err());
    }

    #[test]
    fn pick_asset_finds_exact_name_match() {
        let release = ReleaseInfo {
            tag_name: "v0.2.0".into(),
            assets: vec![
                ReleaseAsset { name: "reins-linux-x86_64.tar.gz".into(), browser_download_url: "u1".into() },
                ReleaseAsset { name: "reins-macos-x86_64.tar.gz".into(), browser_download_url: "u2".into() },
            ],
        };
        let found = pick_asset(&release, "reins-linux-x86_64.tar.gz").unwrap();
        assert_eq!(found.browser_download_url, "u1");
        assert!(pick_asset(&release, "reins-windows-x86_64.tar.gz").is_none());
    }

    #[test]
    fn version_is_newer_compares_semver_tuples() {
        assert!(version_is_newer("0.1.2", "v0.2.0"));
        assert!(version_is_newer("0.1.2", "0.1.3"));
        assert!(!version_is_newer("0.1.2", "v0.1.2"));
        assert!(!version_is_newer("0.2.0", "v0.1.9"));
        assert!(!version_is_newer("0.1.2", "not-a-version"));
    }
}
