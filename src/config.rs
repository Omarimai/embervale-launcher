//! Where the launcher looks for updates and what it installs.
//!
//! Baked defaults, overridable by a `launcher.toml` sitting beside the
//! executable. Overridable rather than hardcoded because the alternative is
//! shipping a new build to move a URL, and the URL is the one thing most likely
//! to move.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Fallback when there is no `launcher.toml`. Points at the release channel the
/// website links to.
///
/// `/releases/latest/download/` is a redirect GitHub maintains, so this URL
/// keeps working across every release without the launcher being rebuilt --
/// which matters, because a launcher already on a player's disk is the one
/// thing a release cannot update.
const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/Omarimai/embervale-client/releases/latest/download/manifest.json";

/// Files the manifest describes are installed here, relative to the launcher.
///
/// A subdirectory rather than the launcher's own directory, so that the set of
/// files the launcher may delete or overwrite never includes the launcher.
const DEFAULT_INSTALL_SUBDIR: &str = "game";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where the manifest lives.
    pub manifest_url: String,
    /// Install root. Relative paths resolve against the launcher's directory.
    pub install_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            manifest_url: DEFAULT_MANIFEST_URL.to_string(),
            install_dir: PathBuf::from(DEFAULT_INSTALL_SUBDIR),
        }
    }
}

impl Config {
    /// Reads `launcher.toml` beside the executable, falling back to defaults.
    ///
    /// A malformed file is reported rather than ignored: silently falling back
    /// to the default URL would send the player to a different channel than the
    /// one they edited the file to reach.
    pub fn load() -> (Self, Option<String>) {
        let base = base_dir();
        let path = base.join("launcher.toml");

        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    return (
                        Self::default(),
                        Some(format!("{} is not valid: {e}", path.display())),
                    );
                }
            },
            Err(_) => Self::default(),
        };

        if let Ok(url) = std::env::var("EMBERVALE_MANIFEST_URL") {
            if !url.is_empty() {
                config.manifest_url = url;
            }
        }

        if config.install_dir.is_relative() {
            config.install_dir = base.join(&config.install_dir);
        }

        (config, None)
    }
}

/// The directory the launcher executable is in.
///
/// Not the working directory: a shortcut can start the launcher from anywhere,
/// and installing the game wherever the shell happened to be pointing is how
/// you end up with the game in `C:\Windows\System32`.
pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
