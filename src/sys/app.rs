// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Interfaces to macOS APIs for interacting with other applications.

use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};
use std::process::Command;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

use accessibility::{AXAttribute, AXAttributeValue, AXError, AXUIElement, AXUIElementAttributes};
pub use accessibility_sys::pid_t;
use accessibility_sys::{
    kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute, kAXStandardWindowSubrole,
    kAXWindowRole,
};
use objc2::rc::Retained;
use objc2::{class, msg_send};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType, CGRect};
use objc2_foundation::NSString;
use redact::Secret;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use super::geometry::CGRectDef;
use super::window_server::WindowServerId;

pub fn running_apps(bundle: Option<String>) -> impl Iterator<Item = (pid_t, AppInfo)> {
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .into_iter()
        .flat_map(move |app| {
            let bundle_id = app.bundle_id()?.to_string();
            if let Some(filter) = &bundle {
                if !bundle_id.contains(filter) {
                    return None;
                }
            }
            Some((app.pid(), AppInfo::from(&*app)))
        })
}

/// Hides or unhides the application. Returns false if the process doesn't
/// exist or macOS refused the request.
pub fn set_app_hidden(pid: pid_t, hidden: bool) -> bool {
    let Some(app) = NSRunningApplication::with_process_id(pid) else {
        return false;
    };
    if hidden { app.hide() } else { app.unhide() }
}

/// Whether the application is hidden, or None if the process doesn't exist.
pub fn is_app_hidden(pid: pid_t) -> Option<bool> {
    NSRunningApplication::with_process_id(pid).map(|app| app.isHidden())
}

/// Activates the application, bringing forward only its main and key windows.
/// Returns false if the process doesn't exist or macOS refused the request.
pub fn activate_app(pid: pid_t) -> bool {
    NSRunningApplication::with_process_id(pid)
        .is_some_and(|app| app.activateWithOptions(NSApplicationActivationOptions::empty()))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchError {
    #[error("bundle id must not be empty")]
    EmptyBundleId,
    #[error("bundle id {0:?} must contain only [A-Za-z0-9.-] and not start with '-'")]
    InvalidBundleId(String),
    #[error("could not start a thread to launch the app: {0}")]
    Spawn(String),
}

/// Launches the application with `/usr/bin/open -b` on a separate thread
/// without blocking. The bundle id must be non-empty and contain only
/// `[A-Za-z0-9.-]`, and must not start with `-`.
pub fn launch_app(bundle_id: &str) -> Result<(), LaunchError> {
    launch_app_then(bundle_id, |_| ())
}

/// Like [`launch_app`], then calls `on_exit` on the launch thread with whether
/// `open` succeeded. `on_exit` is not called if the bundle id is invalid.
pub fn launch_app_then(
    bundle_id: &str,
    on_exit: impl FnOnce(bool) + Send + 'static,
) -> Result<(), LaunchError> {
    launch_app_with(bundle_id, open_bundle, on_exit)
}

type OnExit = Box<dyn FnOnce(bool) + Send>;

/// The callbacks waiting for each bundle id whose `open` is running.
static LAUNCHING: Mutex<BTreeMap<String, Vec<OnExit>>> = Mutex::new(BTreeMap::new());

/// Validates the bundle id, then calls `open` and `on_exit` with its result on
/// a new thread. While `open` runs for a bundle id, launching it again only
/// adds `on_exit` to the callbacks of the running `open`.
fn launch_app_with(
    bundle_id: &str,
    open: impl FnOnce(&str) -> bool + Send + 'static,
    on_exit: impl FnOnce(bool) + Send + 'static,
) -> Result<(), LaunchError> {
    validate_bundle_id(bundle_id)?;
    let mut launching = LAUNCHING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(waiting) = launching.get_mut(bundle_id) {
        info!("Already launching {bundle_id}");
        waiting.push(Box::new(on_exit));
        return Ok(());
    }
    let id = bundle_id.to_owned();
    std::thread::Builder::new()
        .name("launch app".to_owned())
        .spawn(move || {
            let launched = open(&id);
            let waiting = LAUNCHING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id)
                .unwrap_or_default();
            for on_exit in waiting {
                on_exit(launched);
            }
        })
        .map_err(|err| LaunchError::Spawn(err.to_string()))?;
    launching.insert(bundle_id.to_owned(), vec![Box::new(on_exit)]);
    Ok(())
}

/// Runs `/usr/bin/open -b` and returns whether it succeeded.
fn open_bundle(bundle_id: &str) -> bool {
    match Command::new("/usr/bin/open").args(["-b", bundle_id]).output() {
        Ok(out) if out.status.success() => {
            info!("Launched {bundle_id}");
            true
        }
        Ok(out) => {
            error!(
                "open -b {bundle_id} exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            error!("Failed to run open -b {bundle_id}: {e}");
            false
        }
    }
}

/// Checks a bundle id the way [`launch_app`] does, without launching anything.
pub fn validate_bundle_id(bundle_id: &str) -> Result<(), LaunchError> {
    if bundle_id.is_empty() {
        return Err(LaunchError::EmptyBundleId);
    }
    if bundle_id.starts_with('-')
        || !bundle_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(LaunchError::InvalidBundleId(bundle_id.to_owned()));
    }
    Ok(())
}

pub trait NSRunningApplicationExt {
    fn with_process_id(pid: pid_t) -> Option<Retained<Self>>;
    fn pid(&self) -> pid_t;
    fn bundle_id(&self) -> Option<Retained<NSString>>;
    fn localized_name(&self) -> Option<Retained<NSString>>;
}

impl NSRunningApplicationExt for NSRunningApplication {
    fn with_process_id(pid: pid_t) -> Option<Retained<Self>> {
        unsafe {
            // For some reason this binding isn't generated in icrate.
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier:pid]
        }
    }
    fn pid(&self) -> pid_t {
        unsafe { msg_send![self, processIdentifier] }
    }
    fn bundle_id(&self) -> Option<Retained<NSString>> {
        self.bundleIdentifier()
    }
    fn localized_name(&self) -> Option<Retained<NSString>> {
        self.localizedName()
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct AppInfo {
    pub bundle_id: Option<String>,
    pub localized_name: Option<String>,
    /// Whether the application was hidden when this info was read.
    #[serde(default)]
    pub is_hidden: bool,
}

impl From<&NSRunningApplication> for AppInfo {
    fn from(app: &NSRunningApplication) -> Self {
        AppInfo {
            bundle_id: app.bundle_id().as_deref().map(ToString::to_string),
            localized_name: app.localized_name().as_deref().map(ToString::to_string),
            is_hidden: app.isHidden(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WindowInfo {
    pub is_standard: bool,
    // This only gets used for the record/replay feature.
    #[serde(serialize_with = "redact::expose_secret")]
    pub title: Secret<String>,
    #[serde(with = "CGRectDef")]
    pub frame: CGRect,
    pub sys_id: Option<WindowServerId>,
    pub is_resizable: bool,
    /// The macOS Accessibility AXRole, e.g. "AXWindow".
    pub ax_role: String,
    /// The macOS Accessibility AXSubrole, e.g. "AXStandardWindow".
    pub ax_subrole: Option<String>,
}

impl TryFrom<&AXUIElement> for WindowInfo {
    type Error = accessibility::Error;
    fn try_from(element: &AXUIElement) -> Result<Self, accessibility::Error> {
        let role = element.role()?;
        let subrole = match element.subrole() {
            Ok(s) => Some(s),
            Err(accessibility::Error::Ax(e))
                if e == AXError::NoValue || e == AXError::AttributeUnsupported =>
            {
                None
            }
            Err(e) => return Err(e),
        };
        let is_standard = role.to_string() == kAXWindowRole
            && subrole.as_ref().is_some_and(|s| s.to_string() == kAXStandardWindowSubrole);
        let ax_subrole = subrole.map(|s| s.to_string());
        Ok(WindowInfo {
            is_standard,
            title: element.title().map(|t| t.to_string().into()).unwrap_or_default(),
            frame: element.frame()?,
            sys_id: WindowServerId::try_from(element).ok(),
            is_resizable: element.is_settable(&AXAttribute::size())?,
            ax_role: role.to_string(),
            ax_subrole,
        })
    }
}

pub trait AXUIElementExt {
    /// "Enhanced user interface" mode for screen readers and other accessibility apps.
    ///
    /// For most apps this is disabled unless a screen reader is running.
    /// One exception is Firefox, which seems to enable it automatically as soon
    /// as we do anything with accessibility
    /// (https://bugzilla.mozilla.org/show_bug.cgi?id=1845364).
    ///
    /// It seems to do two things:
    ///
    /// * Allows full access to UI elements through the API.
    /// * Animates window moves and resizes. These interfere with Glide animations.
    ///
    /// Other window managers disable this before moving or resizing a window;
    /// see https://issues.chromium.org/issues/40865608.
    fn enhanced_user_interface(&self) -> Result<bool, accessibility::Error>;
    fn set_enhanced_user_interface(&self, enabled: bool) -> Result<(), accessibility::Error>;

    /// Minimizes or unminimizes a window element.
    fn set_minimized(&self, minimized: bool) -> Result<(), accessibility::Error>;

    /// The process the element belongs to.
    fn pid(&self) -> Result<pid_t, accessibility::Error>;

    /// The application with keyboard focus, which is only available on the
    /// system-wide element.
    ///
    /// This follows keyboard focus and not the frontmost application, so it
    /// points at a non-activating panel like Spotlight's while one is open.
    fn focused_application(&self) -> Result<CFRetained<AXUIElement>, accessibility::Error>;

    /// The element with keyboard focus, which is only available on the
    /// system-wide element.
    fn focused_ui_element(&self) -> Result<CFRetained<AXUIElement>, accessibility::Error>;

    fn privacy_sensitive_inspect(&self) -> Inspect<'_>;
}

impl AXUIElementExt for AXUIElement {
    fn enhanced_user_interface(&self) -> Result<bool, accessibility::Error> {
        Ok(self.attribute(&enhanced_ui())?.downcast::<CFBoolean>().is_ok_and(|b| b.value()))
    }
    fn set_enhanced_user_interface(&self, enabled: bool) -> Result<(), accessibility::Error> {
        self.set_attribute(&enhanced_ui(), CFBoolean::new(enabled))
    }

    fn set_minimized(&self, minimized: bool) -> Result<(), accessibility::Error> {
        self.set_attribute(&AXAttribute::minimized(), CFBoolean::new(minimized))
    }

    fn pid(&self) -> Result<pid_t, accessibility::Error> {
        let mut pid = 0;
        // SAFETY: The out parameter is a valid pointer.
        let res = unsafe { self.as_sys().pid(NonNull::from(&mut pid)) };
        if let Some(err) = AXError::from_raw(res) {
            return Err(accessibility::Error::Ax(err));
        }
        Ok(pid)
    }

    fn focused_application(&self) -> Result<CFRetained<AXUIElement>, accessibility::Error> {
        AXUIElement::downcast(
            self.attribute(&system_wide_attribute(kAXFocusedApplicationAttribute))?,
        )
    }

    fn focused_ui_element(&self) -> Result<CFRetained<AXUIElement>, accessibility::Error> {
        AXUIElement::downcast(self.attribute(&system_wide_attribute(kAXFocusedUIElementAttribute))?)
    }

    fn privacy_sensitive_inspect(&self) -> Inspect<'_> {
        Inspect(self)
    }
}

fn enhanced_ui() -> AXAttribute<CFType> {
    AXAttribute::new(&CFString::from_static_str("AXEnhancedUserInterface"))
}

fn system_wide_attribute(name: &'static str) -> AXAttribute<CFType> {
    AXAttribute::new(&CFString::from_static_str(name))
}

pub struct Inspect<'a>(&'a AXUIElement);

impl Debug for Inspect<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let mut st = f.debug_struct("AXWindow");
        for attr in self.0.attribute_names().unwrap().iter() {
            if let Ok(value) = self.0.attribute(&AXAttribute::new(&attr)) {
                st.field(&attr.to_string(), &*value);
            }
        }
        st.finish()
    }
}

pub struct ProcessInfo {
    pub is_xpc: bool,
}

impl ProcessInfo {
    pub fn for_pid(pid: pid_t) -> Result<Self, ()> {
        let psn = ProcessSerialNumber::for_pid(pid)?;

        let mut info = ProcessInfoRec::default();
        info.processInfoLength = size_of::<ProcessInfoRec>() as _;
        if unsafe { GetProcessInformation(&psn, &mut info) } != 0 {
            return Err(());
        }

        Ok(Self {
            is_xpc: info.processType.to_be_bytes() == *b"XPC!",
        })
    }
}

type FourCharCode = u32;
type OSType = FourCharCode;

#[allow(dead_code)]
#[allow(non_snake_case)]
#[repr(C, packed(2))]
#[derive(Default)]
struct ProcessInfoRec {
    processInfoLength: u32,
    processName: *const u8,
    processNumber: ProcessSerialNumber,
    processType: u32,
    processSignature: OSType,
    processMode: u32,
    processLocation: *const u8,
    processSize: u32,
    processFreeMem: u32,
    processLauncher: ProcessSerialNumber,
    processLaunchDate: u32,
    processActiveTime: u32,
    processAppRef: *const u8,
}
const _: () = if size_of::<ProcessInfoRec>() != 72 {
    panic!("unexpected size")
};

#[repr(C)]
#[derive(Default)]
pub(super) struct ProcessSerialNumber {
    high: u32,
    low: u32,
}

impl ProcessSerialNumber {
    pub(super) fn for_pid(pid: pid_t) -> Result<Self, ()> {
        let mut psn = ProcessSerialNumber::default();
        if unsafe { GetProcessForPID(pid, &mut psn) } == 0 {
            Ok(psn)
        } else {
            Err(())
        }
    }

    pub(super) fn pid(&self) -> Result<pid_t, ()> {
        let mut pid = 0;
        if unsafe { GetProcessPID(self, &mut pid) } == 0 {
            Ok(pid)
        } else {
            Err(())
        }
    }
}

type OSErr = i16;
type OSStatus = i32;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // Deprecated in macOS 10.9.
    fn GetProcessForPID(pid: pid_t, psn: *mut ProcessSerialNumber) -> OSStatus;

    // Deprecated in macOS 10.9.
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut pid_t) -> OSStatus;

    // Deprecated in macOS 10.9.
    fn GetProcessInformation(psn: *const ProcessSerialNumber, info: *mut ProcessInfoRec) -> OSErr;
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const MISSING_PID: pid_t = pid_t::MAX;

    #[test]
    fn validate_bundle_id_accepts_valid_ids() {
        assert_eq!(validate_bundle_id("com.apple.TextEdit"), Ok(()));
        assert_eq!(validate_bundle_id("com.example.my-app2"), Ok(()));
    }

    #[test]
    fn validate_bundle_id_rejects_empty() {
        assert_eq!(validate_bundle_id(""), Err(LaunchError::EmptyBundleId));
        assert_eq!(launch_app(""), Err(LaunchError::EmptyBundleId));
    }

    /// Launches with an `open` that reports the ids it was called with instead
    /// of starting anything.
    fn launch_with_fake_open(
        id: &str,
        opened: bool,
    ) -> (Result<(), LaunchError>, mpsc::Receiver<(String, bool)>) {
        let (tx, rx) = mpsc::channel();
        let open_tx = tx.clone();
        let result = launch_app_with(
            id,
            move |id| {
                _ = open_tx.send((id.to_owned(), opened));
                opened
            },
            move |launched| _ = tx.send(("on_exit".to_owned(), launched)),
        );
        (result, rx)
    }

    const NOTHING_SENT: Duration = Duration::from_millis(100);

    #[test]
    fn launch_does_not_open_or_call_back_for_an_invalid_id() {
        for id in ["", "-a", "com.example app"] {
            let (result, rx) = launch_with_fake_open(id, true);
            assert!(result.is_err(), "{id:?}");
            assert!(rx.recv_timeout(NOTHING_SENT).is_err(), "{id:?}");
        }
    }

    #[test]
    fn launch_opens_the_id_and_reports_the_result() {
        for opened in [true, false] {
            let (result, rx) = launch_with_fake_open("com.example.app", opened);
            assert_eq!(result, Ok(()));
            let wait = Duration::from_secs(5);
            assert_eq!(rx.recv_timeout(wait), Ok(("com.example.app".to_owned(), opened)));
            assert_eq!(rx.recv_timeout(wait), Ok(("on_exit".to_owned(), opened)));
        }
    }

    #[test]
    fn launch_does_not_open_an_id_again_while_it_is_opening() {
        const ID: &str = "com.example.slow-open";
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        let open_tx = tx.clone();
        let first_exit_tx = tx.clone();
        let result = launch_app_with(
            ID,
            move |_| {
                _ = open_tx.send("open".to_owned());
                _ = release_rx.recv();
                false
            },
            move |launched| _ = first_exit_tx.send(format!("exit 0 {launched}")),
        );
        assert_eq!(result, Ok(()));
        let wait = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(wait).as_deref(), Ok("open"));

        for i in 1..=3 {
            let open_tx = tx.clone();
            let exit_tx = tx.clone();
            let result = launch_app_with(
                ID,
                move |_| {
                    _ = open_tx.send(format!("open {i}"));
                    true
                },
                move |launched| _ = exit_tx.send(format!("exit {i} {launched}")),
            );
            assert_eq!(result, Ok(()));
        }
        assert!(
            rx.recv_timeout(NOTHING_SENT).is_err(),
            "opened again or exited early"
        );

        // Every launch gets the result of the running open.
        release_tx.send(()).unwrap();
        let exits: Vec<String> = (0..4).map(|_| rx.recv_timeout(wait).unwrap()).collect();
        assert_eq!(
            exits,
            [
                "exit 0 false",
                "exit 1 false",
                "exit 2 false",
                "exit 3 false"
            ]
        );
        drop(tx);

        // Once it has finished, the id is opened again.
        let (result, rx) = launch_with_fake_open(ID, true);
        assert_eq!(result, Ok(()));
        assert_eq!(rx.recv_timeout(wait), Ok((ID.to_owned(), true)));
        assert_eq!(rx.recv_timeout(wait), Ok(("on_exit".to_owned(), true)));
    }

    #[test]
    fn validate_bundle_id_rejects_invalid_characters() {
        for id in [
            "com.example app",
            "com.example;rm",
            "-a",
            "com.example/app",
            "com.example_app",
            "com.exampl\u{e9}",
            "com.example\n",
            "$(id)",
        ] {
            assert_eq!(
                validate_bundle_id(id),
                Err(LaunchError::InvalidBundleId(id.to_owned())),
                "{id:?}"
            );
            assert!(launch_with_fake_open(id, true).0.is_err(), "{id:?}");
        }
    }

    #[test]
    fn validate_bundle_id_accepts_edge_valid_ids() {
        for id in ["a", "A", "0", "com.Example.APP-9", "a-", "a.b.c.d.e.f"] {
            assert_eq!(validate_bundle_id(id), Ok(()), "{id:?}");
        }
        let long = "a.".repeat(500) + "b";
        assert_eq!(validate_bundle_id(&long), Ok(()));
    }

    #[test]
    fn validate_bundle_id_rejects_option_like_and_control_characters() {
        for id in [
            "-",
            "--help",
            "-b",
            " com.example.app",
            "com.example.app ",
            "com.example\tapp",
            "com.example\0app",
            "com.example\rapp",
            "com.example'app",
            "com.example\"app",
            "com.example`id`",
            "com.example|app",
            "com.example&app",
            "com.example*",
            "com.\u{0661}\u{0662}",
            "com.\u{ff41}pp",
            "com.example\u{200b}",
        ] {
            assert_eq!(
                validate_bundle_id(id),
                Err(LaunchError::InvalidBundleId(id.to_owned())),
                "{id:?}"
            );
            assert!(launch_with_fake_open(id, true).0.is_err(), "{id:?}");
        }
    }

    #[test]
    fn launch_error_messages_describe_the_problem() {
        assert_eq!(
            LaunchError::EmptyBundleId.to_string(),
            "bundle id must not be empty"
        );
        assert!(LaunchError::InvalidBundleId("a b".into()).to_string().contains("\"a b\""));
    }

    #[test]
    fn is_app_hidden_is_none_for_invalid_pid() {
        assert_eq!(is_app_hidden(-1), None);
    }

    #[test]
    fn app_control_fails_for_missing_process() {
        assert_eq!(is_app_hidden(MISSING_PID), None);
        assert!(!set_app_hidden(MISSING_PID, true));
        assert!(!set_app_hidden(MISSING_PID, false));
        assert!(!activate_app(MISSING_PID));
    }
}
