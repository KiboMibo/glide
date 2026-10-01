// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Replace an installed bundle with a new one.
//!
//! This runs from the new version's CLI, after the old version has exited.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

const WAIT_FOR_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Wait for the process `pid` to exit.
pub fn wait_for_exit(pid: libc::pid_t) -> anyhow::Result<()> {
    let start = Instant::now();
    // SAFETY: Signal 0 only checks whether the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        if start.elapsed() > WAIT_FOR_EXIT_TIMEOUT {
            bail!("Process {pid} did not exit");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Replace the bundle at `target` with a copy of the one at `source`.
///
/// On failure `target` is left as it was.
pub fn replace_bundle(source: &Path, target: &Path) -> anyhow::Result<()> {
    let incoming = sibling(target, "incoming")?;
    let old = sibling(target, "old")?;
    remove_if_exists(&incoming)?;
    remove_if_exists(&old)?;

    // Copy next to the target first so the final swap is two renames on the
    // same volume.
    let status = Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(&incoming)
        .status()
        .context("Could not run ditto")?;
    if !status.success() {
        bail!("Copying {} to {} failed", source.display(), incoming.display());
    }

    fs::rename(target, &old)
        .with_context(|| format!("Could not move {} aside", target.display()))?;
    if let Err(e) = fs::rename(&incoming, target) {
        fs::rename(&old, target).with_context(|| {
            format!("Could not restore {} after a failed install", target.display())
        })?;
        return Err(e)
            .with_context(|| format!("Could not move new bundle to {}", target.display()));
    }
    if let Err(e) = fs::remove_dir_all(&old) {
        eprintln!("Could not remove {}: {e}", old.display());
    }
    Ok(())
}

/// Launch the bundle at `path` with `args`.
pub fn launch(path: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let status = Command::new("/usr/bin/open")
        .arg("-n")
        .arg(path)
        .arg("--args")
        .args(args)
        .status()
        .context("Could not run open")?;
    if !status.success() {
        bail!("Launching {} failed", path.display());
    }
    Ok(())
}

/// A hidden path next to `path`, like `.Glide.app.<suffix>`.
fn sibling(path: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let name = path.file_name().context("bundle path has no file name")?;
    let mut sibling_name = OsString::from(".");
    sibling_name.push(name);
    sibling_name.push(".");
    sibling_name.push(suffix);
    Ok(path.with_file_name(sibling_name))
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("Could not remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn make_bundle(path: &Path, contents: &str) {
        fs::create_dir_all(path.join("Contents")).unwrap();
        fs::write(path.join("Contents/version"), contents).unwrap();
    }

    fn contents(path: &Path) -> String {
        fs::read_to_string(path.join("Contents/version")).unwrap()
    }

    #[test]
    fn replaces_bundle_and_cleans_up() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("staged/Glide.app");
        let target = dir.path().join("Applications/Glide.app");
        make_bundle(&source, "new");
        make_bundle(&target, "old");

        replace_bundle(&source, &target).unwrap();

        assert_eq!(contents(&target), "new");
        let mut names: Vec<_> = fs::read_dir(dir.path().join("Applications"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["Glide.app"]);
    }

    #[test]
    fn replaces_bundle_despite_leftovers() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("staged/Glide.app");
        let target = dir.path().join("Glide.app");
        make_bundle(&source, "new");
        make_bundle(&target, "old");
        make_bundle(&dir.path().join(".Glide.app.incoming"), "stale");
        make_bundle(&dir.path().join(".Glide.app.old"), "stale");

        replace_bundle(&source, &target).unwrap();

        assert_eq!(contents(&target), "new");
    }

    #[test]
    fn leaves_target_when_source_is_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("Glide.app");
        make_bundle(&target, "old");

        assert!(replace_bundle(&dir.path().join("missing.app"), &target).is_err());

        assert_eq!(contents(&target), "old");
    }

    #[test]
    fn waits_for_exited_process() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        child.wait().unwrap();
        wait_for_exit(pid).unwrap();
    }
}
