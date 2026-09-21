// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Moving windows between spaces using private SkyLight APIs.

use std::ffi::c_int;
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, VerificationError};
use objc2::{msg_send, sel};
use objc2_core_foundation::{CFArray, CFNumber, CFRetained};
use objc2_foundation::{NSArray, NSNumber};

use super::screen::SpaceId;
use super::window_server::WindowServerId;

#[derive(Debug, thiserror::Error)]
pub enum SpaceMoveError {
    #[error("SkyLight does not provide SLSBridgedMoveWindowsToManagedSpaceOperation")]
    Unsupported,
    #[error("the window server does not list the window as owned by the app")]
    NotOwned,
}

/// Asks the window server to move a window to a space.
///
/// The move is asynchronous: it usually completes within 100ms, and there is
/// no result. A space id that does not exist is ignored by the window server.
///
/// The operation is performed by the process that manages spaces, so this
/// works for windows of other processes without the scripting addition.
///
/// Objects SkyLight autoreleases during the call are released before it
/// returns, so it can be called from any thread.
pub fn move_window_to_space(wsid: WindowServerId, space: SpaceId) -> Result<(), SpaceMoveError> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    let cls = CLASS
        .get_or_init(|| {
            let cls = AnyClass::get(c"SLSBridgedMoveWindowsToManagedSpaceOperation")?;
            verify_move_operation(cls).ok()?;
            Some(cls)
        })
        .ok_or(SpaceMoveError::Unsupported)?;
    autoreleasepool(|_| {
        let windows = NSArray::from_retained_slice(&[NSNumber::new_u32(wsid.0)]);
        // SAFETY: verify_move_operation checked the selectors and their signatures.
        unsafe {
            let op: Option<Retained<AnyObject>> =
                msg_send![msg_send![cls, alloc], initWithWindows: &*windows, spaceID: space.get()];
            let op = op.ok_or(SpaceMoveError::Unsupported)?;
            let () = msg_send![&*op, performWithWMBridgeDelegate];
        }
        Ok(())
    })
}

/// Checks that `cls` has the methods `move_window_to_space` sends, with the
/// signatures it uses. Sending a message with a missing selector aborts.
fn verify_move_operation(cls: &AnyClass) -> Result<(), VerificationError> {
    cls.verify_sel::<(&NSArray, u64), *mut AnyObject>(sel!(initWithWindows:spaceID:))?;
    cls.verify_sel::<(), ()>(sel!(performWithWMBridgeDelegate))
}

/// Returns the spaces a window is on. Empty if the window does not exist.
pub fn spaces_for_window(wsid: WindowServerId) -> Vec<SpaceId> {
    const ALL_SPACES: c_int = 0x7;
    let windows = CFArray::from_retained_objects(&[CFNumber::new_i64(wsid.0.into())]);
    // SAFETY: The call returns an owned (+1) array of CFNumbers, per the copy rule.
    let spaces = unsafe {
        let Some(spaces) =
            SLSCopySpacesForWindows(SLSMainConnectionID(), ALL_SPACES, windows.as_opaque())
        else {
            return vec![];
        };
        CFRetained::<CFArray<CFNumber>>::from_raw(spaces.cast())
    };
    spaces.iter().filter_map(|n| space_id_from_raw(n.as_i64()? as u64)).collect()
}

/// Converts a raw space id, as used by SkyLight, to a `SpaceId`.
pub fn space_id_from_raw(id: u64) -> Option<SpaceId> {
    SpaceId::from_raw(id)
}

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    safe fn SLSMainConnectionID() -> c_int;
    fn SLSCopySpacesForWindows(
        cid: c_int,
        selector: c_int,
        windows: &CFArray,
    ) -> Option<NonNull<CFArray>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_id_round_trips() {
        assert_eq!(space_id_from_raw(430).unwrap().get(), 430);
        assert_eq!(space_id_from_raw(430), Some(SpaceId::new(430)));
        assert_eq!(space_id_from_raw(0), None);
    }

    #[test]
    fn space_id_conversion_preserves_extreme_values() {
        for raw in [1, 2, u32::MAX as u64, u32::MAX as u64 + 1, u64::MAX] {
            let space = space_id_from_raw(raw).unwrap();
            assert_eq!(space.get(), raw);
            assert_eq!(space, SpaceId::new(raw));
        }
    }

    #[test]
    fn verification_fails_for_a_class_without_the_methods() {
        let cls = AnyClass::get(c"NSObject").unwrap();
        assert!(verify_move_operation(cls).is_err());
    }

    #[test]
    fn verification_accepts_the_skylight_class_when_present() {
        if let Some(cls) = AnyClass::get(c"SLSBridgedMoveWindowsToManagedSpaceOperation") {
            verify_move_operation(cls).unwrap();
        }
    }

    #[test]
    fn unsupported_error_names_the_missing_class() {
        assert!(
            SpaceMoveError::Unsupported
                .to_string()
                .contains("SLSBridgedMoveWindowsToManagedSpaceOperation")
        );
    }
}
