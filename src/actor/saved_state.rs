// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! State saved on exit and restored on the next launch in the same login
//! session.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::actor::layout::LayoutManager;
use crate::actor::space_manager::SpaceState;
use crate::config::Config;
use crate::sys::session::LoginSessionId;

#[derive(Serialize, Deserialize)]
pub struct SavedState<L = LayoutManager> {
    /// The version of Glide that saved the state.
    pub version: String,
    /// The login session the state was saved in. State is only restored in
    /// the same session.
    pub session: Option<LoginSessionId>,
    pub spaces: SpaceState,
    pub layout: L,
}

impl SavedState {
    /// Save the current state to `path`.
    pub fn save(layout: &LayoutManager, spaces: SpaceState, path: &Path) -> io::Result<()> {
        // State without a session can never be restored.
        let session = LoginSessionId::current()
            .ok_or_else(|| io::Error::other("could not determine the login session"))?;
        let state = SavedState {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            session: Some(session),
            spaces,
            layout,
        };
        let serialized = ron::ser::to_string(&state).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("ron.tmp");
        fs::write(&tmp, serialized)?;
        fs::rename(&tmp, path)
    }

    /// Load the state at `path` without checking the session.
    pub fn load(path: &Path, config: Arc<Config>) -> anyhow::Result<Self> {
        let serialized = fs::read_to_string(path)?;
        Self::parse(&serialized, config)
    }

    /// Take the state saved at `path` if it was saved in `session`.
    ///
    /// The file is moved aside, even if it can't be restored, so it is only
    /// ever restored once. If `session` is unknown, the file is left for a later
    /// launch to check.
    pub fn take(
        path: &Path,
        session: Option<&LoginSessionId>,
        config: Arc<Config>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(session) = session else {
            info!("Not restoring state because the login session is unknown");
            return Ok(None);
        };
        let serialized = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        discard(path)?;
        let state = Self::parse(&serialized, config)
            .with_context(|| format!("Could not restore state saved in {}", path.display()))?;
        if state.session.as_ref() != Some(session) {
            info!(
                "Not restoring state saved by version {} in a different login session",
                state.version
            );
            return Ok(None);
        }
        info!("Restoring state saved by version {}", state.version);
        Ok(Some(state))
    }

    fn parse(serialized: &str, config: Arc<Config>) -> anyhow::Result<Self> {
        let state: SavedState = ron::from_str(serialized)?;
        Ok(SavedState {
            layout: state.layout.with_restored_config(config),
            ..state
        })
    }
}

/// Take the layout saved at `path` by a version of Glide before [`SavedState`],
/// if it was saved after `boot_time`.
///
/// The file is moved aside, even if it can't be restored, so it is only ever
/// restored once.
pub fn take_legacy_layout(
    path: &Path,
    boot_time: Option<SystemTime>,
    config: Arc<Config>,
) -> anyhow::Result<Option<LayoutManager>> {
    let modified = match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(modified) => modified,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let result = match boot_time {
        Some(boot_time) if modified > boot_time => {
            info!("Restoring layout saved by an older version");
            LayoutManager::load(path.to_owned(), config)
                .map(Some)
                .with_context(|| format!("Could not restore layout saved in {}", path.display()))
        }
        _ => {
            info!("Not restoring layout saved by an older version before this boot");
            Ok(None)
        }
    };
    discard(path)?;
    result
}

/// Move the state saved at `path` aside so it is not restored.
pub fn discard(path: &Path) -> io::Result<()> {
    match fs::rename(path, previous_path(path)) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        res => res,
    }
}

fn previous_path(path: &Path) -> PathBuf {
    path.with_extension("prev.ron")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::tempdir;

    use super::*;
    use crate::sys::screen::SpaceId;

    fn spaces() -> SpaceState {
        SpaceState {
            enabled: BTreeSet::from([SpaceId::new(1)]),
            disabled: BTreeSet::from([SpaceId::new(2)]),
            globally_enabled: false,
        }
    }

    fn config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    fn save_in_session(path: &Path, session: &str) {
        let state = SavedState {
            version: "0.0.0".to_owned(),
            session: Some(LoginSessionId::new_for_test(session)),
            spaces: spaces(),
            layout: &LayoutManager::new_for_test(),
        };
        fs::write(path, ron::ser::to_string(&state).unwrap()).unwrap();
    }

    #[test]
    fn take_restores_state_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.ron");
        save_in_session(&path, "a");
        let session = LoginSessionId::new_for_test("a");

        let state = SavedState::take(&path, Some(&session), config()).unwrap().unwrap();
        assert_eq!(state.spaces, spaces());
        assert!(!path.exists());
        assert!(previous_path(&path).exists());

        assert!(SavedState::take(&path, Some(&session), config()).unwrap().is_none());
    }

    #[test]
    fn take_ignores_state_from_another_session() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.ron");
        save_in_session(&path, "a");

        let other = LoginSessionId::new_for_test("b");
        assert!(SavedState::take(&path, Some(&other), config()).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn take_ignores_state_when_session_is_unknown() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.ron");
        save_in_session(&path, "a");

        assert!(SavedState::take(&path, None, config()).unwrap().is_none());
        assert!(path.exists());
    }

    #[test]
    fn take_moves_invalid_state_aside() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.ron");
        fs::write(&path, "not a saved state").unwrap();
        let session = LoginSessionId::new_for_test("a");

        assert!(SavedState::take(&path, Some(&session), config()).is_err());
        assert!(!path.exists());
        assert!(SavedState::take(&path, Some(&session), config()).unwrap().is_none());
    }

    #[test]
    fn legacy_layout_saved_this_boot_is_restored_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("layout.ron");
        LayoutManager::new_for_test().save(path.clone()).unwrap();
        let boot = SystemTime::UNIX_EPOCH;

        assert!(take_legacy_layout(&path, Some(boot), config()).unwrap().is_some());
        assert!(!path.exists());
        assert!(take_legacy_layout(&path, Some(boot), config()).unwrap().is_none());
    }

    #[test]
    fn legacy_layout_saved_before_boot_is_not_restored() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("layout.ron");
        LayoutManager::new_for_test().save(path.clone()).unwrap();
        let boot = SystemTime::now() + std::time::Duration::from_secs(60);

        assert!(take_legacy_layout(&path, Some(boot), config()).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn legacy_layout_is_not_restored_without_boot_time() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("layout.ron");
        LayoutManager::new_for_test().save(path.clone()).unwrap();

        assert!(take_legacy_layout(&path, None, config()).unwrap().is_none());
    }

    #[test]
    fn save_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.ron");
        SavedState::save(&LayoutManager::new_for_test(), spaces(), &path).unwrap();

        let state = SavedState::load(&path, config()).unwrap();
        assert_eq!(state.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(state.session, LoginSessionId::current());
        assert_eq!(state.spaces, spaces());
    }
}
