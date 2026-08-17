//! The update manifest: what the current release consists of.
//!
//! One static JSON file, served next to the payload. Static because a launcher
//! that needs a live service to tell it what to download is a launcher that
//! stops working when that service does, and because "upload files, upload a
//! manifest" is a release process that cannot get out of step with itself.

use serde::Deserialize;

/// How long to wait on the manifest before giving up. Short: this runs before
/// the player can do anything, so a hung request looks like a hung launcher.
const TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Human-readable release name, shown in the launcher. "0.4.1", "beta 3".
    pub version: String,
    /// Everything the install consists of. Anything present locally but absent
    /// here is left alone -- the launcher does not own the whole directory.
    pub files: Vec<FileEntry>,
    /// Relative to the install root, the thing PLAY starts.
    pub launch: String,
    /// Optional release notes for the news panel, newest first.
    #[serde(default)]
    pub news: Vec<NewsItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    /// Destination, relative to the install root. Forward slashes.
    pub path: String,
    /// Lowercase hex SHA-256 of the file's contents.
    pub sha256: String,
    /// Expected size in bytes. Checked before hashing, because a size
    /// mismatch is the same answer for far less work.
    pub size: u64,
    /// Absolute URL to download from.
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsItem {
    pub title: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub body: String,
}

/// Fetches and parses the manifest.
pub fn fetch(url: &str) -> Result<Manifest, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(TIMEOUT_SECS)))
        .build()
        .new_agent();

    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("cannot reach {url}: {e}"))?;

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("cannot read the manifest: {e}"))?;

    serde_json::from_str(&body).map_err(|e| format!("the manifest is not valid: {e}"))
}

impl FileEntry {
    /// Rejects paths that would escape the install root.
    ///
    /// The manifest is fetched over the network and then used to decide where
    /// to write files, so "../../windows/system32/..." has to be impossible
    /// rather than merely unlikely. Absolute paths and drive letters go too.
    pub fn safe_relative_path(&self) -> Result<std::path::PathBuf, String> {
        let raw = self.path.replace('\\', "/");
        if raw.is_empty() {
            return Err("the manifest has an entry with an empty path".into());
        }
        let mut out = std::path::PathBuf::new();
        for part in raw.split('/') {
            match part {
                "" | "." => continue,
                ".." => return Err(format!("the manifest path {raw:?} escapes the install")),
                p if p.contains(':') => {
                    return Err(format!("the manifest path {raw:?} is not relative"));
                }
                p => out.push(p),
            }
        }
        if out.as_os_str().is_empty() {
            return Err(format!("the manifest path {raw:?} names nothing"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            sha256: String::new(),
            size: 0,
            url: String::new(),
        }
    }

    #[test]
    fn ordinary_paths_are_kept() {
        let p = entry("data/spells.json").safe_relative_path().unwrap();
        assert_eq!(p, std::path::PathBuf::from("data").join("spells.json"));
    }

    #[test]
    fn traversal_is_refused() {
        for bad in ["../evil.exe", "a/../../evil.exe", "..\\evil.exe"] {
            assert!(
                entry(bad).safe_relative_path().is_err(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn absolute_paths_are_refused() {
        for bad in ["C:/windows/system32/x.dll", "C:\\windows\\x.dll"] {
            assert!(
                entry(bad).safe_relative_path().is_err(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn empty_names_are_refused() {
        assert!(entry("").safe_relative_path().is_err());
        assert!(entry("./").safe_relative_path().is_err());
    }
}
