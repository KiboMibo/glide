// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Check for new releases and install them when the user asks.
//!
//! Which release to offer is decided by a manifest on the website. For version
//! a.b.c the updater tries, in order,
//!
//! ```text
//! <manifest base>/a.b.c.json
//! <manifest base>/a.b.json
//! <manifest base>/a.json
//! <manifest base>/default.json
//! ```
//!
//! and uses the first one that exists. Only a 404 moves on to the next; any
//! other failure means no update is offered. The manifest names a release on
//! GitHub, whose zipped app bundle is downloaded, verified, and handed to the
//! new version's `glide update apply` to swap in once this process exits.

pub mod install;

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tracing::{Span, error, info, warn};

use crate::actor::{status, wm_controller};
use crate::sys::bundle::glide_bundle;

const DEFAULT_MANIFEST_BASE: &str = "https://glidewm.org/update/v1";
const DEFAULT_RELEASES_API: &str = "https://api.github.com/repos/glide-wm/glide/releases";
const MANIFEST_BASE_ENV: &str = "GLIDE_UPDATE_MANIFEST_URL";
const RELEASES_API_ENV: &str = "GLIDE_UPDATE_RELEASES_URL";
/// Set to override the version this process believes it is, for testing updates.
const CURRENT_VERSION_ENV: &str = "GLIDE_UPDATE_CURRENT_VERSION";

/// Code signing requirement a bundle must satisfy to be installed.
pub const CODE_REQUIREMENT: &str = "anchor apple generic and certificate leaf[subject.OU] = \
     \"77455J9D7M\" and identifier \"org.glidewm.glide\"";

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest to wait before looking at the clock again.
///
/// Waits are measured on a clock that stops while the computer sleeps, so wait
/// in short steps to notice when a check is due after waking.
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug)]
pub enum Request {
    Check,
    Install,
}

pub type Sender = mpsc::Sender<Request>;

/// The updater's state, as reported to the CLI.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UpdaterState {
    /// Why updates are disabled, if they are.
    pub disabled: Option<String>,
    /// The number of checks completed.
    pub checks: u64,
    pub available: Option<String>,
    pub installing: bool,
    /// The error from the last check or install, if it failed.
    pub error: Option<String>,
}

/// A handle to send requests to the updater and read its state.
#[derive(Clone)]
pub struct UpdaterHandle {
    tx: Sender,
    state: Arc<Mutex<UpdaterState>>,
}

impl UpdaterHandle {
    pub fn sender(&self) -> Sender {
        self.tx.clone()
    }

    pub fn check(&self) {
        _ = self.tx.send(Request::Check);
    }

    /// Start installing the available update.
    ///
    /// The state shows the install in progress from when this returns.
    pub fn install(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.installing = true;
            state.error = None;
        }
        _ = self.tx.send(Request::Install);
    }

    pub fn state(&self) -> UpdaterState {
        self.state.lock().unwrap().clone()
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        UpdaterHandle {
            tx: mpsc::channel().0,
            state: Default::default(),
        }
    }
}

/// What the status menu should show about updates.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    Available(Version),
    Installing(Version),
    Failed(Version),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// Parse `a.b.c`, with an optional leading `v` and ignoring any
    /// prerelease or build suffix.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let core = s.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("package version should parse")
    }

    /// Manifest names to try for this version, most specific first.
    fn manifest_names(self) -> [String; 4] {
        let Version(major, minor, patch) = self;
        [
            format!("{major}.{minor}.{patch}"),
            format!("{major}.{minor}"),
            format!("{major}"),
            "default".to_owned(),
        ]
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Deserialize, Debug)]
struct Manifest {
    enabled: bool,
    /// `"latest"` for the latest GitHub release, or a version number.
    version: String,
}

#[derive(Deserialize, Debug)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize, Debug)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Available {
    version: Version,
    url: String,
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    }
}

/// Pick the asset for `arch` from `release` if it is newer than `current`.
fn select_update(
    release: &Release,
    current: Version,
    arch: &str,
) -> anyhow::Result<Option<Available>> {
    let version = Version::parse(&release.tag_name)
        .with_context(|| format!("Could not parse release tag {:?}", release.tag_name))?;
    if version <= current {
        return Ok(None);
    }
    let suffix = format!("_{arch}.app.zip");
    let asset = release
        .assets
        .iter()
        .find(|a| a.name.ends_with(&suffix))
        .with_context(|| format!("Release {version} has no asset ending in {suffix}"))?;
    Ok(Some(Available {
        version,
        url: asset.browser_download_url.clone(),
    }))
}

fn release_url(api: &str, manifest: &Manifest) -> anyhow::Result<String> {
    if manifest.version == "latest" {
        return Ok(format!("{api}/latest"));
    }
    let version = Version::parse(&manifest.version)
        .with_context(|| format!("Could not parse manifest version {:?}", manifest.version))?;
    Ok(format!("{api}/tags/v{version}"))
}

enum Fetched {
    Found,
    NotFound,
}

/// Download `url` to `dest`.
fn fetch(url: &str, dest: &Path) -> anyhow::Result<Fetched> {
    let output = Command::new("/usr/bin/curl")
        .args(["--silent", "--show-error", "--location"])
        // Give up on a stalled transfer, but not a slow one.
        .args(["--speed-limit", "1", "--speed-time", "60"])
        .args(["--user-agent", concat!("Glide/", env!("CARGO_PKG_VERSION"))])
        .args(["--write-out", "%{http_code}", "--output"])
        .arg(dest)
        .arg(url)
        .output()
        .context("Could not run curl")?;
    if !output.status.success() {
        bail!(
            "Fetching {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "200" => Ok(Fetched::Found),
        "404" => Ok(Fetched::NotFound),
        code => bail!("Fetching {url} failed with HTTP status {code}"),
    }
}

fn fetch_json<T: for<'de> Deserialize<'de>>(url: &str) -> anyhow::Result<Option<T>> {
    let file = tempfile::NamedTempFile::new()?;
    match fetch(url, file.path())? {
        Fetched::NotFound => Ok(None),
        Fetched::Found => {
            let body = fs::read(file.path())?;
            let value =
                serde_json::from_slice(&body).with_context(|| format!("Could not parse {url}"))?;
            Ok(Some(value))
        }
    }
}

/// Check that the bundle at `path` is signed by us.
pub fn verify_signature(path: &Path) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(format!("-R={CODE_REQUIREMENT}"))
        .arg(path)
        .output()
        .context("Could not run codesign")?;
    if !output.status.success() {
        bail!(
            "{} failed signature verification: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn is_writable(path: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid C string.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

pub struct Updater {
    manifest_base: String,
    releases_api: String,
    current: Version,
    config_path: Option<PathBuf>,
    /// Whether an update was handed off to the new version's installer.
    handed_off: bool,
    status_tx: status::Sender,
    wm_tx: wm_controller::Sender,
    available: Option<Available>,
    state: Arc<Mutex<UpdaterState>>,
}

impl Updater {
    /// Start checking for updates on a background thread.
    ///
    /// `config_path` is the config file Glide was started with, if not the
    /// default. It is checked against the new version before installing, and
    /// passed to the new version when it is launched.
    pub fn spawn(
        config_path: Option<PathBuf>,
        status_tx: status::Sender,
        wm_tx: wm_controller::Sender,
    ) -> UpdaterHandle {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(UpdaterState::default()));
        let current = std::env::var(CURRENT_VERSION_ENV)
            .ok()
            .and_then(|v| Version::parse(&v))
            .unwrap_or_else(Version::current);
        let updater = Updater {
            manifest_base: std::env::var(MANIFEST_BASE_ENV)
                .unwrap_or_else(|_| DEFAULT_MANIFEST_BASE.to_owned()),
            releases_api: std::env::var(RELEASES_API_ENV)
                .unwrap_or_else(|_| DEFAULT_RELEASES_API.to_owned()),
            current,
            config_path,
            handed_off: false,
            status_tx,
            wm_tx,
            available: None,
            state: state.clone(),
        };
        std::thread::Builder::new()
            .name("updater".to_owned())
            .spawn(move || updater.run(rx))
            .unwrap();
        UpdaterHandle { tx, state }
    }

    fn run(mut self, rx: mpsc::Receiver<Request>) {
        let bundle = match installed_bundle() {
            Ok(bundle) => bundle,
            Err(e) => {
                info!("Not checking for updates: {e:#}");
                self.state.lock().unwrap().disabled = Some(format!("{e:#}"));
                return;
            }
        };
        let mut next_check = SystemTime::now() + FIRST_CHECK_DELAY;
        loop {
            let wait = next_check
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO)
                .min(MAX_WAIT);
            match rx.recv_timeout(wait) {
                Ok(Request::Check) => self.check(),
                Ok(Request::Install) => self.install(&bundle),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if SystemTime::now() >= next_check {
                        self.check();
                        next_check = SystemTime::now() + CHECK_INTERVAL;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn check(&mut self) {
        let result = self.find_update();
        {
            let mut state = self.state.lock().unwrap();
            state.checks += 1;
            state.error = result.as_ref().err().map(|e| format!("{e:#}"));
            if let Ok(available) = &result {
                state.available = available.as_ref().map(|a| a.version.to_string());
            }
        }
        match result {
            Ok(Some(available)) => {
                info!("Update available: {}", available.version);
                self.send_status(Some(UpdateStatus::Available(available.version)));
                self.available = Some(available);
            }
            Ok(None) => {
                info!("No update available");
                if self.available.take().is_some() {
                    self.send_status(None);
                }
            }
            Err(e) => warn!("Checking for updates failed: {e:#}"),
        }
    }

    fn find_update(&self) -> anyhow::Result<Option<Available>> {
        let mut manifest = None;
        for name in self.current.manifest_names() {
            let url = format!("{}/{name}.json", self.manifest_base);
            if let Some(m) = fetch_json::<Manifest>(&url)? {
                info!("Using update manifest {url}: {m:?}");
                manifest = Some(m);
                break;
            }
        }
        let Some(manifest) = manifest else {
            return Ok(None);
        };
        if !manifest.enabled {
            return Ok(None);
        }
        let url = release_url(&self.releases_api, &manifest)?;
        let release: Release =
            fetch_json(&url)?.with_context(|| format!("Release not found at {url}"))?;
        select_update(&release, self.current, arch())
    }

    fn install(&mut self, bundle: &Path) {
        if self.handed_off {
            info!("Ignoring install request; the update was already handed off");
            return;
        }
        let Some(available) = self.available.clone() else {
            warn!("Install requested with no update available");
            let mut state = self.state.lock().unwrap();
            state.installing = false;
            state.error = Some("No update available".to_owned());
            return;
        };
        self.send_status(Some(UpdateStatus::Installing(available.version)));
        {
            let mut state = self.state.lock().unwrap();
            state.installing = true;
            state.error = None;
        }
        let result = self.stage_and_hand_off(&available, bundle);
        {
            let mut state = self.state.lock().unwrap();
            state.installing = result.is_ok();
            state.error = result.as_ref().err().map(|e| format!("{e:#}"));
        }
        match result {
            Ok(()) => {
                info!("Handed off update to {}; exiting", available.version);
                _ = self.wm_tx.send((
                    Span::current(),
                    wm_controller::WmEvent::Command(wm_controller::WmCommand::Wm(
                        wm_controller::WmCmd::SaveAndExit,
                    )),
                ));
            }
            Err(e) => {
                error!("Installing update {} failed: {e:#}", available.version);
                self.send_status(Some(UpdateStatus::Failed(available.version)));
            }
        }
    }

    fn stage_and_hand_off(&mut self, available: &Available, bundle: &Path) -> anyhow::Result<()> {
        let dir = dirs::cache_dir()
            .context("Could not find the cache directory")?
            .join("org.glidewm.glide/updates");
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;

        let zip = dir.join("Glide.app.zip");
        info!("Downloading {}", available.url);
        match fetch(&available.url, &zip)? {
            Fetched::Found => {}
            Fetched::NotFound => bail!("{} was not found", available.url),
        }
        let staged = dir.join("staged");
        run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip).arg(&staged))?;
        let app = staged.join("Glide.app");
        verify_signature(&app)?;

        let cli = app.join("Contents/MacOS/glide");
        let version = run(Command::new(&cli).arg("--version"))?;
        if Version::parse(version.trim().trim_start_matches("glide ")) != Some(available.version) {
            bail!(
                "Downloaded bundle reports version {version:?}, expected {}",
                available.version
            );
        }

        let config_path = (self.config_path.clone())
            .unwrap_or_else(crate::config::config_path)
            .canonicalize()
            .ok();
        // Without a config file, the new version uses its own defaults.
        if let Some(path) = config_path {
            let mut verify = Command::new(&cli);
            verify.args(["config", "verify", "--config"]).arg(path);
            run(&mut verify).context("Your config is not compatible with the new version")?;
        }

        let log = update_log()?;
        let mut install = Command::new(&cli);
        install
            .args(["update", "apply", "--target"])
            .arg(bundle)
            .arg("--wait-pid")
            .arg(std::process::id().to_string())
            .arg("--")
            .args(self.relaunch_args())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // Keep running after this process exits.
            .process_group(0);
        install.spawn().context("Could not start the installer")?;
        self.handed_off = true;
        Ok(())
    }

    fn relaunch_args(&self) -> Vec<OsString> {
        // Updating restarts Glide without the user asking to, so restore even
        // if auto_restore is off.
        let mut args = vec!["--restore".into()];
        if let Some(path) = &self.config_path {
            args.extend(["--config".into(), path.clone().into_os_string()]);
        }
        args
    }

    fn send_status(&self, status: Option<UpdateStatus>) {
        self.status_tx.send(status::Event::UpdateStatusChanged(status));
    }
}

/// The bundle this process is running from, if it can be updated in place.
fn installed_bundle() -> anyhow::Result<PathBuf> {
    let Ok(bundle) = glide_bundle() else {
        bail!("not running from an app bundle");
    };
    let path = PathBuf::from(bundle.bundlePath().to_string());
    verify_signature(&path)?;
    let parent = path.parent().context("bundle has no parent directory")?;
    if !is_writable(&path) || !is_writable(parent) {
        bail!("{} is not writable", path.display());
    }
    Ok(path)
}

fn update_log() -> anyhow::Result<File> {
    let path = crate::config::data_dir().join("update.log");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

/// Run `cmd` and return its stdout, failing if it exits unsuccessfully.
fn run(cmd: &mut Command) -> anyhow::Result<String> {
    let output = cmd.output().with_context(|| format!("Could not run {cmd:?}"))?;
    if !output.status.success() {
        bail!(
            "{cmd:?} failed: {}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("0.2.15"), Some(Version(0, 2, 15)));
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.2.16-dev+abc"), Some(Version(0, 2, 16)));
        assert_eq!(Version::parse("0.2"), None);
        assert_eq!(Version::parse("0.2.3.4"), None);
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn orders_versions_numerically() {
        assert!(Version(0, 2, 10) > Version(0, 2, 9));
        assert!(Version(0, 3, 0) > Version(0, 2, 15));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
    }

    #[test]
    fn current_version_parses() {
        Version::current();
    }

    #[test]
    fn manifest_names_go_from_specific_to_general() {
        assert_eq!(
            Version(0, 2, 15).manifest_names(),
            ["0.2.15", "0.2", "0", "default"]
        );
    }

    #[test]
    fn release_url_for_latest_and_pinned_versions() {
        let api = "https://api.example/releases";
        let latest = Manifest {
            enabled: true,
            version: "latest".into(),
        };
        assert_eq!(release_url(api, &latest).unwrap(), format!("{api}/latest"));
        let pinned = Manifest {
            enabled: true,
            version: "0.2.17".into(),
        };
        assert_eq!(release_url(api, &pinned).unwrap(), format!("{api}/tags/v0.2.17"));
        let bad = Manifest {
            enabled: true,
            version: "soon".into(),
        };
        assert!(release_url(api, &bad).is_err());
    }

    #[test]
    fn manifest_requires_enabled() {
        assert!(serde_json::from_str::<Manifest>(r#"{"version": "latest"}"#).is_err());
        let m: Manifest =
            serde_json::from_str(r#"{"enabled": false, "version": "latest", "extra": 1}"#).unwrap();
        assert!(!m.enabled);
    }

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.to_owned(),
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: (*name).to_owned(),
                    browser_download_url: format!("https://example/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn selects_asset_for_arch_when_newer() {
        let release = release(
            "v0.2.16",
            &[
                "Glide_0.2.16_aarch64.dmg",
                "Glide_0.2.16_aarch64.app.zip",
                "Glide_0.2.16_x64.app.zip",
            ],
        );
        let update = select_update(&release, Version(0, 2, 15), "x64").unwrap().unwrap();
        assert_eq!(update.version, Version(0, 2, 16));
        assert_eq!(update.url, "https://example/Glide_0.2.16_x64.app.zip");
    }

    #[test]
    fn no_update_when_not_newer() {
        let release = release("v0.2.15", &["Glide_0.2.15_aarch64.app.zip"]);
        assert_eq!(
            select_update(&release, Version(0, 2, 15), "aarch64").unwrap(),
            None
        );
        assert_eq!(
            select_update(&release, Version(0, 2, 16), "aarch64").unwrap(),
            None
        );
    }

    #[test]
    fn error_when_asset_missing() {
        let release = release("v0.2.16", &["Glide_0.2.16_aarch64.dmg"]);
        assert!(select_update(&release, Version(0, 2, 15), "aarch64").is_err());
    }

    #[test]
    fn parses_github_release() {
        let json = r#"{
            "tag_name": "v0.2.16",
            "draft": false,
            "assets": [{
                "name": "Glide_0.2.16_aarch64.app.zip",
                "browser_download_url": "https://github.com/glide-wm/glide/releases/download/v0.2.16/Glide_0.2.16_aarch64.app.zip",
                "size": 1
            }]
        }"#;
        let release: Release = serde_json::from_str(json).unwrap();
        let update = select_update(&release, Version(0, 2, 15), "aarch64").unwrap().unwrap();
        assert_eq!(update.version, Version(0, 2, 16));
    }

    #[test]
    fn install_without_update_clears_installing() {
        let handle = UpdaterHandle::new_for_test();
        handle.install();
        let (status_tx, _status_rx) = crate::actor::channel();
        let (wm_tx, _wm_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut updater = Updater {
            manifest_base: String::new(),
            releases_api: String::new(),
            current: Version::current(),
            config_path: None,
            handed_off: false,
            status_tx,
            wm_tx,
            available: None,
            state: handle.state.clone(),
        };
        updater.install(Path::new("/Applications/Glide.app"));
        let state = handle.state();
        assert!(!state.installing);
        assert!(state.error.is_some());
    }
}
