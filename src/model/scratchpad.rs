// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scratchpad windows: named floating windows that are shown and hidden on
//! demand.

use std::collections::BTreeMap;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use serde::{Deserialize, Serialize};

use crate::actor::app::{WindowId, pid_t};

/// A rectangle expressed as fractions of a screen's size.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FractionalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl FractionalRect {
    /// x=0.1, y=0.1, width=0.8, height=0.8 — used when `frame` is not set.
    pub const DEFAULT: FractionalRect = FractionalRect {
        x: 0.1,
        y: 0.1,
        width: 0.8,
        height: 0.8,
    };

    const MIN_SIZE: f64 = 0.05;

    /// Clamps values to [0, 1], width/height to at least 0.05, and keeps
    /// x+width and y+height within 1. NaN is treated as the lower bound.
    pub fn validated(self) -> FractionalRect {
        let clamp = |v: f64, lo: f64, hi: f64| if v.is_nan() { lo } else { v.clamp(lo, hi) };
        let width = clamp(self.width, Self::MIN_SIZE, 1.0);
        let height = clamp(self.height, Self::MIN_SIZE, 1.0);
        FractionalRect {
            x: clamp(self.x, 0.0, 1.0 - width),
            y: clamp(self.y, 0.0, 1.0 - height),
            width,
            height,
        }
    }

    /// The rectangle inside `screen`.
    pub fn to_frame(&self, screen: CGRect) -> CGRect {
        CGRect {
            origin: CGPoint {
                x: screen.origin.x + self.x * screen.size.width,
                y: screen.origin.y + self.y * screen.size.height,
            },
            size: CGSize {
                width: self.width * screen.size.width,
                height: self.height * screen.size.height,
            },
        }
    }
}

/// Facts about the current scratchpad window, gathered by the Reactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchpadWindowState {
    pub app_hidden: bool,
    pub focused: bool,
    pub on_active_space: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchpadAction {
    Launch,
    Show(WindowId),
    Hide(WindowId),
}

/// Identifies the pending show that one launch created.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingShowId(u64);

#[derive(Debug)]
struct PendingShow {
    id: PendingShowId,
    /// The bundle id that was launched.
    launch: String,
}

/// Registry of scratchpad windows by name.
#[derive(Debug, Default)]
pub struct Scratchpads {
    windows: BTreeMap<String, (WindowId, FractionalRect)>,
    pending_show: BTreeMap<String, PendingShow>,
    next_pending_id: u64,
}

impl Scratchpads {
    /// Registers a window under a name. If a live window is already registered
    /// under the name, keeps it and returns false.
    pub fn register(&mut self, name: &str, wid: WindowId, frame: FractionalRect) -> bool {
        if let Some((existing, _)) = self.windows.get(name)
            && *existing != wid
        {
            return false;
        }
        self.windows.retain(|n, (w, _)| *w != wid || n == name);
        self.windows.insert(name.to_owned(), (wid, frame));
        true
    }

    /// Forgets a destroyed window. Returns its name if it was a scratchpad.
    pub fn remove_window(&mut self, wid: WindowId) -> Option<String> {
        let name = self.name_of(wid)?.to_owned();
        self.windows.remove(&name);
        Some(name)
    }

    /// Forgets all windows of an application that has terminated.
    pub fn remove_app(&mut self, pid: pid_t) {
        self.windows.retain(|_, (wid, _)| wid.pid != pid);
    }

    pub fn name_of(&self, wid: WindowId) -> Option<&str> {
        self.windows.iter().find(|(_, (w, _))| *w == wid).map(|(n, _)| n.as_str())
    }

    pub fn window(&self, name: &str) -> Option<(WindowId, FractionalRect)> {
        self.windows.get(name).copied()
    }

    pub fn is_scratchpad(&self, wid: WindowId) -> bool {
        self.name_of(wid).is_some()
    }

    /// Forgets the windows and pending shows of names that `keep` rejects.
    pub fn retain_names(&mut self, keep: impl Fn(&str) -> bool) {
        self.windows.retain(|name, _| keep(name));
        self.pending_show.retain(|name, _| keep(name));
    }

    /// Hide if the window is visible, focused and on the active space;
    /// otherwise Show. Without a window, returns Launch. If there is a bundle
    /// id to `launch`, also remembers to show the window once it is
    /// registered, replacing an earlier pending show of the name.
    pub fn toggle(
        &mut self,
        name: &str,
        launch: Option<&str>,
        state: impl Fn(WindowId) -> ScratchpadWindowState,
    ) -> ScratchpadAction {
        let Some((wid, _)) = self.window(name) else {
            match launch {
                Some(launch) => {
                    let id = PendingShowId(self.next_pending_id);
                    self.next_pending_id += 1;
                    let launch = launch.to_owned();
                    self.pending_show.insert(name.to_owned(), PendingShow { id, launch });
                }
                None => {
                    self.pending_show.remove(name);
                }
            }
            return ScratchpadAction::Launch;
        };
        match state(wid) {
            ScratchpadWindowState {
                app_hidden: false,
                focused: true,
                on_active_space: true,
            } => ScratchpadAction::Hide(wid),
            _ => ScratchpadAction::Show(wid),
        }
    }

    /// Returns true once if a show was pending for the name (after Launch).
    pub fn take_pending_show(&mut self, name: &str) -> bool {
        self.pending_show.remove(name).is_some()
    }

    /// The show pending for the name, if any.
    pub fn pending_show(&self, name: &str) -> Option<PendingShowId> {
        self.pending_show.get(name).map(|pending| pending.id)
    }

    /// Drops the pending show `id` if it is still pending, and returns its
    /// name. A later launch of the same name has a different id and is kept.
    pub fn cancel_pending_show(&mut self, id: PendingShowId) -> Option<String> {
        let name = self.pending_show.iter().find(|(_, p)| p.id == id)?.0.clone();
        self.pending_show.remove(&name);
        Some(name)
    }

    /// Drops the pending shows that launched `bundle_id`.
    pub fn cancel_pending_shows_for_app(&mut self, bundle_id: &str) {
        self.pending_show.retain(|_, p| !p.launch.eq_ignore_ascii_case(bundle_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn state(app_hidden: bool, focused: bool, on_active_space: bool) -> ScratchpadWindowState {
        ScratchpadWindowState {
            app_hidden,
            focused,
            on_active_space,
        }
    }

    const L: Option<&str> = Some("com.example.app");

    const VISIBLE: ScratchpadWindowState = ScratchpadWindowState {
        app_hidden: false,
        focused: true,
        on_active_space: true,
    };

    #[test]
    fn default_frame_on_screen() {
        assert_eq!(
            FractionalRect::DEFAULT.to_frame(rect(0., 0., 1000., 800.)),
            rect(100., 80., 800., 640.)
        );
    }

    #[test]
    fn to_frame_respects_screen_origin() {
        assert_eq!(
            FractionalRect::DEFAULT.to_frame(rect(-1000., 25., 1000., 800.)),
            rect(-900., 105., 800., 640.)
        );
    }

    #[test]
    fn validated_keeps_valid_rect() {
        assert_eq!(FractionalRect::DEFAULT.validated(), FractionalRect::DEFAULT);
    }

    #[test]
    fn validated_clamps_out_of_bounds() {
        let r = FractionalRect {
            x: -0.5,
            y: 0.9,
            width: 2.0,
            height: 0.5,
        }
        .validated();
        assert_eq!(
            r,
            FractionalRect {
                x: 0.0,
                y: 0.5,
                width: 1.0,
                height: 0.5
            }
        );

        let r = FractionalRect {
            x: 1.5,
            y: 0.0,
            width: 0.0,
            height: -1.0,
        }
        .validated();
        assert_eq!(
            r,
            FractionalRect {
                x: 0.95,
                y: 0.0,
                width: 0.05,
                height: 0.05
            }
        );
    }

    #[test]
    fn validated_replaces_nan() {
        let r = FractionalRect {
            x: f64::NAN,
            y: f64::NAN,
            width: f64::NAN,
            height: f64::NAN,
        }
        .validated();
        assert_eq!(
            r,
            FractionalRect {
                x: 0.0,
                y: 0.0,
                width: 0.05,
                height: 0.05
            }
        );
    }

    #[test]
    fn toggle_without_window_launches_and_shows_once() {
        let mut s = Scratchpads::default();
        assert_eq!(s.toggle("k", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert!(s.take_pending_show("k"));
        assert!(!s.take_pending_show("k"));
        assert!(!s.take_pending_show("other"));
    }

    #[test]
    fn toggle_hides_visible_focused_window() {
        let mut s = Scratchpads::default();
        let wid = WindowId::new(1, 1);
        s.register("k", wid, FractionalRect::DEFAULT);
        assert_eq!(s.toggle("k", L, |_| VISIBLE), ScratchpadAction::Hide(wid));
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn toggle_shows_when_any_condition_is_false() {
        let mut s = Scratchpads::default();
        let wid = WindowId::new(1, 1);
        s.register("k", wid, FractionalRect::DEFAULT);
        for st in [
            state(true, true, true),
            state(false, false, true),
            state(false, true, false),
        ] {
            assert_eq!(
                s.toggle("k", L, |w| {
                    assert_eq!(w, wid);
                    st
                }),
                ScratchpadAction::Show(wid),
                "{st:?}"
            );
        }
    }

    #[test]
    fn register_does_not_replace_live_window() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(1, 2);
        let frame = FractionalRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        };
        assert!(s.register("k", a, FractionalRect::DEFAULT));
        assert!(!s.register("k", b, frame));
        assert_eq!(s.window("k"), Some((a, FractionalRect::DEFAULT)));
        assert!(!s.is_scratchpad(b));

        // Re-registering the same window updates its frame.
        assert!(s.register("k", a, frame));
        assert_eq!(s.window("k"), Some((a, frame)));
    }

    #[test]
    fn register_moves_window_to_new_name() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        assert!(s.register("k", a, FractionalRect::DEFAULT));
        assert!(s.register("j", a, FractionalRect::DEFAULT));
        assert_eq!(s.name_of(a), Some("j"));
        assert_eq!(s.window("k"), None);
    }

    #[test]
    fn remove_window_forgets_it() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(1, 2);
        s.register("k", a, FractionalRect::DEFAULT);
        assert_eq!(s.remove_window(b), None);
        assert_eq!(s.remove_window(a), Some("k".to_owned()));
        assert!(!s.is_scratchpad(a));
        assert_eq!(s.window("k"), None);
        assert_eq!(s.remove_window(a), None);

        // The name is free for a new window.
        assert!(s.register("k", b, FractionalRect::DEFAULT));
        assert_eq!(s.toggle("k", L, |_| VISIBLE), ScratchpadAction::Hide(b));
    }

    #[test]
    fn remove_app_forgets_only_its_windows() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(2, 1);
        s.register("k", a, FractionalRect::DEFAULT);
        s.register("j", b, FractionalRect::DEFAULT);
        s.remove_app(1);
        assert_eq!(s.window("k"), None);
        assert_eq!(s.name_of(a), None);
        assert_eq!(s.window("j"), Some((b, FractionalRect::DEFAULT)));
        assert_eq!(s.toggle("k", L, |_| VISIBLE), ScratchpadAction::Launch);
    }

    fn frac(x: f64, y: f64, width: f64, height: f64) -> FractionalRect {
        FractionalRect { x, y, width, height }
    }

    #[test]
    fn to_frame_scales_each_axis_by_its_own_screen_dimension() {
        assert_eq!(
            frac(0.25, 0.5, 0.5, 0.25).to_frame(rect(0., 0., 1440., 900.)),
            rect(360., 450., 720., 225.)
        );
    }

    #[test]
    fn to_frame_on_screen_with_negative_origin() {
        assert_eq!(
            frac(0.25, 0.5, 0.5, 0.25).to_frame(rect(-1440., -900., 1440., 900.)),
            rect(-1080., -450., 720., 225.)
        );
    }

    #[test]
    fn full_screen_rect_matches_screen() {
        let screen = rect(-300., 40., 1920., 1080.);
        assert_eq!(frac(0., 0., 1., 1.).to_frame(screen), screen);
    }

    #[test]
    fn validated_clamps_infinities() {
        assert_eq!(
            frac(
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY
            )
            .validated(),
            frac(0.0, 0.0, 1.0, 0.05)
        );
        assert_eq!(
            frac(f64::NEG_INFINITY, f64::INFINITY, 0.5, 0.5).validated(),
            frac(0.0, 0.5, 0.5, 0.5)
        );
    }

    #[test]
    fn validated_keeps_values_on_the_bounds() {
        let r = frac(0.95, 0.0, 0.05, 1.0);
        assert_eq!(r.validated(), r);
    }

    #[test]
    fn validated_rect_always_fits_the_screen() {
        let values = [
            f64::NEG_INFINITY,
            -1.0,
            -0.0,
            0.0,
            0.03,
            0.05,
            0.5,
            0.95,
            1.0,
            2.0,
            f64::INFINITY,
            f64::NAN,
        ];
        for &x in &values {
            for &y in &values {
                for &width in &values {
                    for &height in &values {
                        let r = frac(x, y, width, height).validated();
                        let ctx = || format!("{:?} -> {r:?}", frac(x, y, width, height));
                        assert!((0.05..=1.0).contains(&r.width), "{}", ctx());
                        assert!((0.05..=1.0).contains(&r.height), "{}", ctx());
                        assert!(r.x >= 0.0 && r.x + r.width <= 1.0, "{}", ctx());
                        assert!(r.y >= 0.0 && r.y + r.height <= 1.0, "{}", ctx());
                        assert_eq!(r.validated(), r, "not idempotent: {}", ctx());
                    }
                }
            }
        }
    }

    #[test]
    fn toggle_hides_only_when_all_conditions_hold() {
        let mut s = Scratchpads::default();
        let wid = WindowId::new(1, 1);
        s.register("k", wid, FractionalRect::DEFAULT);
        for app_hidden in [false, true] {
            for focused in [false, true] {
                for on_active_space in [false, true] {
                    let st = state(app_hidden, focused, on_active_space);
                    let expected = if st == VISIBLE {
                        ScratchpadAction::Hide(wid)
                    } else {
                        ScratchpadAction::Show(wid)
                    };
                    assert_eq!(s.toggle("k", L, |_| st), expected, "{st:?}");
                }
            }
        }
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn toggle_with_window_does_not_request_pending_show() {
        let mut s = Scratchpads::default();
        let wid = WindowId::new(1, 1);
        s.register("k", wid, FractionalRect::DEFAULT);
        assert_eq!(
            s.toggle("k", L, |_| state(true, false, false)),
            ScratchpadAction::Show(wid)
        );
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn toggle_uses_the_window_registered_under_that_name() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(2, 1);
        s.register("k", a, FractionalRect::DEFAULT);
        s.register("j", b, FractionalRect::DEFAULT);
        let hidden_only_a = |w: WindowId| state(w == a, true, true);
        assert_eq!(s.toggle("k", L, hidden_only_a), ScratchpadAction::Show(a));
        assert_eq!(s.toggle("j", L, hidden_only_a), ScratchpadAction::Hide(b));
    }

    #[test]
    fn repeated_launch_keeps_a_single_pending_show_per_name() {
        let mut s = Scratchpads::default();
        assert_eq!(s.toggle("k", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert_eq!(s.toggle("k", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert!(!s.take_pending_show("j"));
        assert!(s.take_pending_show("k"));
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn pending_show_survives_window_registration() {
        let mut s = Scratchpads::default();
        let wid = WindowId::new(1, 1);
        assert_eq!(s.toggle("k", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert!(s.register("k", wid, FractionalRect::DEFAULT));
        assert!(s.take_pending_show("k"));
        assert_eq!(s.toggle("k", L, |_| VISIBLE), ScratchpadAction::Hide(wid));
    }

    #[test]
    fn registering_the_same_window_twice_keeps_one_entry() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        assert!(s.register("k", a, FractionalRect::DEFAULT));
        assert!(s.register("k", a, FractionalRect::DEFAULT));
        assert_eq!(s.name_of(a), Some("k"));
        assert_eq!(s.remove_window(a), Some("k".to_owned()));
        assert!(!s.is_scratchpad(a));
        assert_eq!(s.window("k"), None);
    }

    #[test]
    fn register_under_occupied_name_keeps_window_under_old_name() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(1, 2);
        assert!(s.register("k", a, FractionalRect::DEFAULT));
        assert!(s.register("j", b, FractionalRect::DEFAULT));
        assert!(!s.register("j", a, FractionalRect::DEFAULT));
        assert_eq!(s.name_of(a), Some("k"));
        assert_eq!(s.window("j"), Some((b, FractionalRect::DEFAULT)));
    }

    #[test]
    fn remove_unknown_window_or_app_changes_nothing() {
        let mut s = Scratchpads::default();
        assert_eq!(s.remove_window(WindowId::new(1, 1)), None);
        s.remove_app(1);

        let a = WindowId::new(1, 1);
        s.register("k", a, FractionalRect::DEFAULT);
        assert_eq!(s.remove_window(WindowId::new(2, 1)), None);
        assert_eq!(s.remove_window(WindowId::new(1, 2)), None);
        s.remove_app(2);
        assert_eq!(s.window("k"), Some((a, FractionalRect::DEFAULT)));
        assert!(s.is_scratchpad(a));
    }

    #[test]
    fn retain_names_forgets_windows_and_pending_shows_of_other_names() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(2, 1);
        s.register("k", a, FractionalRect::DEFAULT);
        s.register("j", b, FractionalRect::DEFAULT);
        assert_eq!(s.toggle("l", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert_eq!(s.toggle("m", L, |_| unreachable!()), ScratchpadAction::Launch);
        s.retain_names(|name| name == "j" || name == "m");
        assert!(!s.is_scratchpad(a));
        assert_eq!(s.window("j"), Some((b, FractionalRect::DEFAULT)));
        assert!(!s.take_pending_show("l"));
        assert!(s.take_pending_show("m"));
    }

    #[test]
    fn toggle_without_launch_leaves_no_pending_show() {
        let mut s = Scratchpads::default();
        assert_eq!(s.toggle("k", L, |_| unreachable!()), ScratchpadAction::Launch);
        assert_eq!(s.toggle("k", None, |_| unreachable!()), ScratchpadAction::Launch);
        assert_eq!(s.pending_show("k"), None);
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn cancelling_a_pending_show_keeps_a_later_launch() {
        let mut s = Scratchpads::default();
        s.toggle("k", L, |_| unreachable!());
        let first = s.pending_show("k").unwrap();
        s.toggle("k", L, |_| unreachable!());
        let second = s.pending_show("k").unwrap();
        assert_ne!(first, second);
        assert_eq!(s.cancel_pending_show(first), None);
        assert_eq!(s.pending_show("k"), Some(second));
        assert_eq!(s.cancel_pending_show(second), Some("k".to_owned()));
        assert_eq!(s.pending_show("k"), None);
        assert!(!s.take_pending_show("k"));
    }

    #[test]
    fn cancelling_a_taken_show_does_nothing() {
        let mut s = Scratchpads::default();
        s.toggle("k", L, |_| unreachable!());
        let id = s.pending_show("k").unwrap();
        assert!(s.take_pending_show("k"));
        assert_eq!(s.cancel_pending_show(id), None);
    }

    #[test]
    fn closed_app_cancels_only_its_pending_shows() {
        let mut s = Scratchpads::default();
        s.toggle("k", Some("com.Example.K"), |_| unreachable!());
        s.toggle("j", Some("com.example.j"), |_| unreachable!());
        s.cancel_pending_shows_for_app("com.example.k");
        assert_eq!(s.pending_show("k"), None);
        assert!(s.pending_show("j").is_some());
    }

    #[test]
    fn remove_app_forgets_every_window_of_the_app() {
        let mut s = Scratchpads::default();
        let a = WindowId::new(1, 1);
        let b = WindowId::new(1, 2);
        s.register("k", a, FractionalRect::DEFAULT);
        s.register("j", b, FractionalRect::DEFAULT);
        s.remove_app(1);
        assert!(!s.is_scratchpad(a));
        assert!(!s.is_scratchpad(b));
        assert_eq!(s.window("j"), None);
    }
}
