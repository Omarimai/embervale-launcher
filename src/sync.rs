//! Bringing the install in line with the manifest.
//!
//! Verify, download what differs, verify again. The launcher is the only thing
//! standing between a half-finished download and a player reporting a crash
//! that never happened to anyone else, so nothing is trusted because it arrived
//! -- a file counts as installed when its bytes hash to what the manifest says.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::manifest::{FileEntry, Manifest};

/// Read buffer. Large enough that hashing a multi-gigabyte install is bound by
/// the disk rather than by syscalls.
const CHUNK: usize = 64 * 1024;

/// Connections should fail fast; transfers should not. Splitting the two is
/// what stops a slow but working download from being cut off at some arbitrary
/// deadline while an unreachable host still gives up promptly.
const CONNECT_TIMEOUT_SECS: u64 = 15;

/// What the worker tells the UI as it goes.
pub enum Update {
    /// A line for the status area.
    Status(String),
    /// Bytes transferred out of bytes planned, for the progress bar.
    Progress { done: u64, total: u64 },
}

/// Everything that has to be fetched, and how many bytes that is.
pub struct Plan {
    pub missing: Vec<FileEntry>,
    pub bytes: u64,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Decides what to download, without downloading anything.
///
/// Reports progress per file because hashing a large install is slow enough to
/// look like a hang, and "checking 40 of impatient" is the difference between
/// waiting and force-quitting.
pub fn plan(
    manifest: &Manifest,
    install_dir: &Path,
    report: &mut dyn FnMut(Update),
) -> Result<Plan, String> {
    let mut missing = Vec::new();
    let mut bytes = 0u64;
    let total = manifest.files.len();

    for (i, entry) in manifest.files.iter().enumerate() {
        report(Update::Status(format!(
            "Checking {} of {total}: {}",
            i + 1,
            entry.path
        )));

        let rel = entry.safe_relative_path()?;
        let dest = install_dir.join(&rel);

        if !matches_entry(&dest, entry) {
            bytes += entry.size;
            missing.push(entry.clone());
        }
    }

    Ok(Plan { missing, bytes })
}

/// True when the file on disk is already the one the manifest describes.
///
/// Size first: it is one metadata read, and it rules out most of the ways a
/// file can be wrong for none of the cost of hashing it.
fn matches_entry(dest: &Path, entry: &FileEntry) -> bool {
    let Ok(meta) = fs::metadata(dest) else {
        return false;
    };
    if !meta.is_file() || meta.len() != entry.size {
        return false;
    }
    match hash_file(dest) {
        Ok(sum) => sum.eq_ignore_ascii_case(&entry.sha256),
        Err(_) => false,
    }
}

/// Downloads and installs everything in the plan.
pub fn apply(
    plan: &Plan,
    install_dir: &Path,
    report: &mut dyn FnMut(Update),
) -> Result<(), String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)))
        .build()
        .new_agent();

    let total = plan.bytes;
    let mut done = 0u64;

    for entry in &plan.missing {
        report(Update::Status(format!("Downloading {}", entry.path)));

        let rel = entry.safe_relative_path()?;
        let dest = install_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }

        download_one(&agent, entry, &dest, done, total, report)?;
        done += entry.size;
        report(Update::Progress { done, total });
    }

    Ok(())
}

/// Streams one file to a `.part` beside its destination, hashing as it goes,
/// and moves it into place only once the hash matches.
///
/// Downloading straight onto the destination is what makes a connection dropped
/// at 90% indistinguishable from a corrupt install: the old file is gone, the
/// new one is wrong, and the size looks plausible.
fn download_one(
    agent: &ureq::Agent,
    entry: &FileEntry,
    dest: &Path,
    done_before: u64,
    total: u64,
    report: &mut dyn FnMut(Update),
) -> Result<(), String> {
    let part = dest.with_extension("part");

    let mut response = agent
        .get(&entry.url)
        .call()
        .map_err(|e| format!("cannot download {}: {e}", entry.path))?;

    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(&part)
        .map_err(|e| format!("cannot write {}: {e}", part.display()))?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut written = 0u64;

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("{} was cut short: {e}", entry.path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        written += n as u64;
        report(Update::Progress {
            done: done_before + written.min(entry.size),
            total,
        });
    }

    file.flush()
        .map_err(|e| format!("cannot finish {}: {e}", part.display()))?;
    drop(file);

    let sum = hex(hasher.finalize().as_slice());
    if !sum.eq_ignore_ascii_case(&entry.sha256) {
        let _ = fs::remove_file(&part);
        return Err(format!(
            "{} arrived corrupt (expected {}, got {sum})",
            entry.path, entry.sha256
        ));
    }
    if written != entry.size {
        let _ = fs::remove_file(&part);
        return Err(format!(
            "{} is {written} bytes, the manifest says {}",
            entry.path, entry.size
        ));
    }

    // Windows will not rename onto an existing file, so the old one goes first.
    // Safe at this point: the replacement is downloaded and verified.
    if dest.exists() {
        fs::remove_file(dest).map_err(|e| format!("cannot replace {}: {e}", dest.display()))?;
    }
    fs::rename(&part, dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))?;

    Ok(())
}

/// SHA-256 of a file's contents, lowercase hex.
pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Resolves the executable PLAY should start, refusing anything outside the
/// install root for the same reason file paths are checked.
pub fn launch_target(manifest: &Manifest, install_dir: &Path) -> Result<PathBuf, String> {
    let entry = FileEntry {
        path: manifest.launch.clone(),
        sha256: String::new(),
        size: 0,
        url: String::new(),
    };
    Ok(install_dir.join(entry.safe_relative_path()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn hashing_matches_a_known_vector() {
        let dir = std::env::temp_dir().join("embervale-launcher-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            hash_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_never_matches() {
        let entry = FileEntry {
            path: "nope.bin".into(),
            sha256: "00".into(),
            size: 0,
            url: String::new(),
        };
        assert!(!matches_entry(Path::new("no-such-file-here.bin"), &entry));
    }
}
