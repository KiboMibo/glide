// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
use std::mem;
use std::time::Duration;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use tokio::sync::mpsc;

use super::TransactionId;
use crate::actor::app::{AppThreadHandle, Request, WindowId};
use crate::sys::timer::Timer;

pub type Sender = mpsc::UnboundedSender<Message>;
pub type Receiver = mpsc::UnboundedReceiver<Message>;

#[derive(Debug)]
pub enum Message {
    Replace(Animation),
    SkipToEnd(Animation),
    /// One frame of a scroll animation, computed by the caller. Each window
    /// moves straight to its `finish` frame.
    ScrollFrame(Animation),
    /// The last frame of a scroll animation. Every window that took part in it
    /// gets a sized frame at its last target, then its animation is ended.
    ScrollEnd(Animation),
    /// The reactor accepted a frame for a window from outside Glide, e.g. a
    /// user resize. If the window takes part in the scroll animation, it ends
    /// at this frame.
    ScrollWindowFrame(WindowId, CGRect),
}

impl Message {
    pub fn into_animation(self) -> Animation {
        match self {
            Message::Replace(animation)
            | Message::SkipToEnd(animation)
            | Message::ScrollFrame(animation)
            | Message::ScrollEnd(animation) => animation,
            Message::ScrollWindowFrame(..) => Animation::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct AnimationManager {
    active: Option<ActiveAnimation>,
    /// Windows with a scroll animation begun and not yet ended.
    scrolling: BTreeMap<WindowId, ScrollingWindow>,
}

/// The last frame sent to a window in a scroll animation.
#[derive(Debug)]
struct ScrollingWindow {
    handle: AppThreadHandle,
    frame: CGRect,
    txid: TransactionId,
}

#[derive(Debug)]
struct ActiveAnimation {
    animation: Animation,
    next_frame: u32,
}

#[derive(Debug)]
pub struct Animation {
    interval: Duration,
    frames: u32,
    windows: Vec<AnimatedWindow>,
}

#[derive(Debug)]
struct AnimatedWindow {
    handle: AppThreadHandle,
    wid: WindowId,
    start: CGRect,
    finish: CGRect,
    is_focus: bool,
    txid: TransactionId,
}

impl AnimatedWindow {
    fn frame_after(&self, frame: u32, total_frames: u32) -> CGRect {
        if frame == 0 {
            return if self.is_focus {
                CGRect {
                    origin: self.start.origin,
                    size: self.finish.size,
                }
            } else {
                self.start
            };
        }

        let t = f64::from(frame) / f64::from(total_frames);
        let mut rect = get_frame(self.start, self.finish, t);
        if self.is_focus || frame * 2 >= total_frames {
            rect.size = self.finish.size;
        } else {
            rect.size = self.start.size;
        }
        rect
    }
}

impl AnimationManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run(mut rx: Receiver) {
        let mut manager = Self::new();
        let mut tick_timer = Timer::manual();

        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else {
                        manager.finish_active();
                        manager.end_scroll();
                        break;
                    };
                    if let Some(delay) = manager.handle_message(message) {
                        tick_timer.set_next_fire(delay);
                    }
                }
                _ = tick_timer.next(), if manager.active.is_some() => {
                    if let Some(delay) = manager.tick() {
                        tick_timer.set_next_fire(delay);
                    }
                }
            }
        }
    }

    pub fn handle_message(&mut self, message: Message) -> Option<Duration> {
        match message {
            Message::Replace(_) | Message::SkipToEnd(_) => self.end_scroll(),
            Message::ScrollFrame(_) | Message::ScrollEnd(_) => self.finish_active(),
            Message::ScrollWindowFrame(..) => (),
        }
        match message {
            Message::Replace(animation) => {
                self.active = match self.active.take() {
                    Some(active) => Some(active.replace_with(animation)),
                    None => ActiveAnimation::start(animation),
                };
                self.active.as_ref().map(|active| active.animation.interval)
            }
            Message::SkipToEnd(animation) => {
                self.finish_active();
                animation.skip_to_end();
                None
            }
            Message::ScrollFrame(animation) => {
                for window in &animation.windows {
                    let sent = ScrollingWindow {
                        handle: window.handle.clone(),
                        frame: window.finish,
                        txid: window.txid,
                    };
                    if self.scrolling.insert(window.wid, sent).is_none() {
                        _ = window.handle.send(Request::BeginWindowAnimation(window.wid));
                    }
                    _ = window.handle.send(Request::AnimationFrame {
                        wid: window.wid,
                        frame: window.finish,
                        set_size: window.start.size != window.finish.size,
                        txid: window.txid,
                    });
                }
                None
            }
            Message::ScrollEnd(animation) => {
                for window in &animation.windows {
                    if let Some(sent) = self.scrolling.get_mut(&window.wid) {
                        sent.frame = window.finish;
                        sent.txid = window.txid;
                    } else {
                        _ = window.handle.send(Request::SetWindowFrame(
                            window.wid,
                            window.finish,
                            window.txid,
                        ));
                    }
                }
                self.end_scroll();
                None
            }
            Message::ScrollWindowFrame(wid, frame) => {
                if let Some(sent) = self.scrolling.get_mut(&wid) {
                    sent.frame = frame;
                }
                None
            }
        }
    }

    /// Ends the scroll animation of every begun window. Each first gets a sized
    /// frame at its last target: EndWindowAnimation re-applies the last sized
    /// frame with retries, and without this one that would be a stale
    /// mid-scroll frame.
    fn end_scroll(&mut self) {
        for (wid, ScrollingWindow { handle, frame, txid }) in mem::take(&mut self.scrolling) {
            _ = handle.send(Request::AnimationFrame {
                wid,
                frame,
                set_size: true,
                txid,
            });
            _ = handle.send(Request::EndWindowAnimation(wid));
        }
    }

    pub fn tick(&mut self) -> Option<Duration> {
        let active = self.active.as_mut()?;
        active.send_next_frame();
        if active.is_complete() {
            let active = self.active.take().expect("animation disappeared while ticking");
            active.animation.end();
            None
        } else {
            Some(active.animation.interval)
        }
    }

    fn finish_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.animation.skip_to_end_and_end();
        }
    }
}

impl ActiveAnimation {
    fn start(animation: Animation) -> Option<Self> {
        if animation.is_empty() {
            return None;
        }
        animation.begin();
        Some(Self { animation, next_frame: 1 })
    }

    fn replace_with(self, mut next: Animation) -> Self {
        let current = self.current_frames();
        let continuing = next.patch_starts_from(&current);
        next.begin_windows_not_in(&continuing);
        next.carry_over(self.animation, &current);
        Self { animation: next, next_frame: 1 }
    }

    fn send_next_frame(&mut self) {
        self.animation.send_frame(self.next_frame);
        self.next_frame += 1;
    }

    fn is_complete(&self) -> bool {
        self.next_frame > self.animation.frames
    }

    fn current_frames(&self) -> Vec<(WindowId, CGRect)> {
        let frame = self.next_frame.saturating_sub(1);
        self.animation
            .windows
            .iter()
            .map(|window| (window.wid, window.frame_after(frame, self.animation.frames)))
            .collect()
    }
}

impl Animation {
    pub fn new() -> Self {
        const FPS: f64 = 100.0;
        const DURATION: f64 = 0.30;
        let interval = Duration::from_secs_f64(1.0 / FPS);
        Animation {
            interval,
            frames: (DURATION * FPS).round() as u32,
            windows: vec![],
        }
    }

    pub fn add_window(
        &mut self,
        handle: &AppThreadHandle,
        wid: WindowId,
        start: CGRect,
        finish: CGRect,
        is_focus: bool,
        txid: TransactionId,
    ) {
        self.windows.push(AnimatedWindow {
            handle: handle.clone(),
            wid,
            start,
            finish,
            is_focus,
            txid,
        });
    }

    pub fn skip_to_end(&self) {
        for window in &self.windows {
            _ = window
                .handle
                .send(Request::SetWindowFrame(window.wid, window.finish, window.txid));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    fn begin(&self) {
        self.begin_windows_not_in(&[]);
    }

    fn begin_windows_not_in(&self, skip: &[WindowId]) {
        for window in &self.windows {
            if skip.contains(&window.wid) {
                continue;
            }
            _ = window.handle.send(Request::BeginWindowAnimation(window.wid));
            if window.is_focus {
                let frame = CGRect {
                    origin: window.start.origin,
                    size: window.finish.size,
                };
                _ = window.handle.send(Request::AnimationFrame {
                    wid: window.wid,
                    frame,
                    set_size: true,
                    txid: window.txid,
                });
            }
        }
    }

    fn finish_all(&self) {
        for window in &self.windows {
            _ = window.handle.send(Request::AnimationFrame {
                wid: window.wid,
                frame: window.finish,
                set_size: true,
                txid: window.txid,
            });
            _ = window.handle.send(Request::EndWindowAnimation(window.wid));
        }
    }

    fn send_frame(&self, frame: u32) {
        let t = f64::from(frame) / f64::from(self.frames);
        for window in &self.windows {
            let mut rect = get_frame(window.start, window.finish, t);
            // Don't animate size, too slow. Resize halfway through and again at
            // the end, in case it got clipped during the animation.
            let set_size = frame * 2 == self.frames || frame == self.frames;
            if set_size {
                rect.size = window.finish.size;
            }
            _ = window.handle.send(Request::AnimationFrame {
                wid: window.wid,
                frame: rect,
                set_size,
                txid: window.txid,
            });
        }
    }

    fn end(&self) {
        for window in &self.windows {
            _ = window.handle.send(Request::EndWindowAnimation(window.wid));
        }
    }

    fn patch_starts_from(&mut self, current_frames: &[(WindowId, CGRect)]) -> Vec<WindowId> {
        let mut continuing = Vec::new();
        for &(wid, current_frame) in current_frames {
            let Some(window) = self.windows.iter_mut().find(|window| window.wid == wid) else {
                continue;
            };
            window.start = current_frame;
            continuing.push(wid);
        }
        continuing
    }

    /// Pull in windows from a previous animation that this one doesn't mention,
    /// so they keep animating from their current position toward their original
    /// destination instead of jumping there. Their animations are already begun,
    /// so we only adopt them, patching their start to the current frame.
    fn carry_over(&mut self, previous: Animation, current_frames: &[(WindowId, CGRect)]) {
        for mut window in previous.windows {
            if self.windows.iter().any(|existing| existing.wid == window.wid) {
                continue;
            }
            if let Some(&(_, current_frame)) =
                current_frames.iter().find(|(wid, _)| *wid == window.wid)
            {
                window.start = current_frame;
            }
            self.windows.push(window);
        }
    }

    fn skip_to_end_and_end(self) {
        // Finish every window: jump it to its final frame and end its animation.
        self.finish_all();
    }
}

fn get_frame(a: CGRect, b: CGRect, t: f64) -> CGRect {
    let s = ease(t);
    CGRect {
        origin: CGPoint {
            x: blend(a.origin.x, b.origin.x, s),
            y: blend(a.origin.y, b.origin.y, s),
        },
        size: CGSize {
            width: blend(a.size.width, b.size.width, s),
            height: blend(a.size.height, b.size.height, s),
        },
    }
}

fn ease(t: f64) -> f64 {
    if t < 0.5 {
        (1.0 - f64::sqrt(1.0 - f64::powi(2.0 * t, 2))) / 2.0
    } else {
        (f64::sqrt(1.0 - f64::powi(-2.0 * t + 2.0, 2)) + 1.0) / 2.0
    }
}

fn blend(a: f64, b: f64, s: f64) -> f64 {
    (1.0 - s) * a + s * b
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    fn rect(origin_x: f64, origin_y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(origin_x, origin_y), CGSize::new(width, height))
    }

    fn animation(handle: &AppThreadHandle, wid: WindowId, from: CGRect, to: CGRect) -> Animation {
        let mut animation = Animation::new();
        animation.add_window(handle, wid, from, to, false, TransactionId::default());
        animation
    }

    fn collect_requests(
        rx: &mut mpsc::UnboundedReceiver<(tracing::Span, Request)>,
    ) -> Vec<Request> {
        let mut requests = Vec::new();
        while let Ok((_, request)) = rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    fn assert_set_window_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::SetWindowFrame(req_wid, req_frame, txid) => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected SetWindowFrame, got {request:?}"),
        }
    }

    fn assert_animation_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame: req_frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert!(*set_size, "expected a set_size frame");
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    fn assert_animation_pos(request: &Request, wid: WindowId, pos: CGPoint) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(frame.origin, pos);
                assert!(!*set_size, "expected a position-only frame");
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    #[test]
    fn replacement_uses_last_animated_frame_for_continuing_windows() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert!(matches!(
            collect_requests(&mut rx).as_slice(),
            [Request::BeginWindowAnimation(req_wid)] if *req_wid == wid
        ));

        manager.tick();
        let continuing_frame = manager.active.as_ref().unwrap().current_frames()[0].1;
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, continuing_frame.origin);

        manager.handle_message(Message::Replace(second));
        assert!(collect_requests(&mut rx).is_empty());

        let resumed_start = manager.active.as_ref().unwrap().animation.windows[0].start;
        assert_eq!(resumed_start, continuing_frame);

        manager.tick();
        let expected_next = get_frame(resumed_start, rect(80.0, 90.0, 10.0, 10.0), 1.0 / 30.0);
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, expected_next.origin);
    }

    fn animation_contains(manager: &AnimationManager, wid: WindowId) -> bool {
        manager
            .active
            .as_ref()
            .is_some_and(|active| active.animation.windows.iter().any(|w| w.wid == wid))
    }

    #[test]
    fn replacement_only_restarts_changed_windows() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let wid3 = WindowId::new(1, 3);
        let mut first = Animation::new();
        first.add_window(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        first.add_window(
            &handle,
            wid2,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(60.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        let mut second = Animation::new();
        second.add_window(
            &handle,
            wid1,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.add_window(
            &handle,
            wid3,
            rect(20.0, 0.0, 10.0, 10.0),
            rect(90.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert_eq!(collect_requests(&mut rx).len(), 2);
        manager.handle_message(Message::Replace(second));

        // Only the brand-new window (wid3) is begun. The dropped window (wid2)
        // is carried over so it keeps animating toward its original destination
        // instead of jumping there.
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], Request::BeginWindowAnimation(req_wid) if req_wid == wid3));
        assert!(animation_contains(&manager, wid2));

        let carried = manager
            .active
            .as_ref()
            .unwrap()
            .animation
            .windows
            .iter()
            .find(|w| w.wid == wid2)
            .unwrap();
        assert_eq!(carried.finish, rect(60.0, 60.0, 10.0, 10.0));
    }

    #[test]
    fn skip_to_end_finishes_active_animation_and_applies_new_layout() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        manager.handle_message(Message::SkipToEnd(second));

        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 4);
        assert!(matches!(requests[0], Request::BeginWindowAnimation(req_wid) if req_wid == wid));
        assert_animation_frame(&requests[1], wid, rect(50.0, 60.0, 10.0, 10.0));
        assert!(matches!(requests[2], Request::EndWindowAnimation(req_wid) if req_wid == wid));
        assert_set_window_frame(&requests[3], wid, rect(80.0, 90.0, 10.0, 10.0));
    }

    #[test]
    fn scroll_frames_begin_each_window_once_and_end_all_of_them() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let wid3 = WindowId::new(1, 3);
        let mut manager = AnimationManager::new();

        let frame1 = rect(10.0, 0.0, 10.0, 10.0);
        manager.handle_message(Message::ScrollFrame(animation(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            frame1,
        )));
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 2);
        assert!(matches!(requests[0], Request::BeginWindowAnimation(w) if w == wid1));
        assert_animation_pos(&requests[1], wid1, frame1.origin);

        let frame2 = rect(20.0, 0.0, 10.0, 10.0);
        manager.handle_message(Message::ScrollFrame(animation(&handle, wid1, frame1, frame2)));
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 1);
        assert_animation_pos(&requests[0], wid1, frame2.origin);

        // wid2 moves on the last frame only; wid3 does not move at the end.
        let mut last = animation(&handle, wid1, frame2, rect(30.0, 0.0, 10.0, 10.0));
        last.add_window(
            &handle,
            wid2,
            rect(0.0, 20.0, 10.0, 10.0),
            rect(5.0, 20.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        manager.handle_message(Message::ScrollFrame(animation(
            &handle,
            wid3,
            rect(0.0, 40.0, 10.0, 10.0),
            rect(1.0, 40.0, 10.0, 10.0),
        )));
        collect_requests(&mut rx);
        manager.handle_message(Message::ScrollEnd(last));
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 5, "{requests:?}");
        assert_set_window_frame(&requests[0], wid2, rect(5.0, 20.0, 10.0, 10.0));
        assert_animation_frame(&requests[1], wid1, rect(30.0, 0.0, 10.0, 10.0));
        assert!(matches!(requests[2], Request::EndWindowAnimation(w) if w == wid1));
        assert_animation_frame(&requests[3], wid3, rect(1.0, 40.0, 10.0, 10.0));
        assert!(matches!(requests[4], Request::EndWindowAnimation(w) if w == wid3));
        assert!(manager.scrolling.is_empty());
    }

    #[test]
    fn scroll_window_frame_sets_the_final_frame_of_a_begun_window_only() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::ScrollFrame(animation(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(10.0, 0.0, 10.0, 10.0),
        )));
        collect_requests(&mut rx);

        let user_frame = rect(10.0, 0.0, 8.0, 10.0);
        manager.handle_message(Message::ScrollWindowFrame(wid1, user_frame));
        manager.handle_message(Message::ScrollWindowFrame(wid2, user_frame));
        assert!(collect_requests(&mut rx).is_empty());

        manager.handle_message(Message::ScrollEnd(Animation::new()));
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_animation_frame(&requests[0], wid1, user_frame);
        assert!(matches!(requests[1], Request::EndWindowAnimation(w) if w == wid1));
    }

    #[test]
    fn layout_animation_ends_scroll_animation_first() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::ScrollFrame(animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(10.0, 0.0, 10.0, 10.0),
        )));
        collect_requests(&mut rx);

        manager.handle_message(Message::Replace(animation(
            &handle,
            wid,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(50.0, 0.0, 10.0, 10.0),
        )));
        let requests = collect_requests(&mut rx);
        assert_animation_frame(&requests[0], wid, rect(10.0, 0.0, 10.0, 10.0));
        assert!(matches!(requests[1], Request::EndWindowAnimation(w) if w == wid));
        assert!(matches!(requests[2], Request::BeginWindowAnimation(w) if w == wid));
    }

    #[test]
    fn scroll_frame_finishes_layout_animation_first() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 0.0, 10.0, 10.0),
        )));
        collect_requests(&mut rx);

        manager.handle_message(Message::ScrollFrame(animation(
            &handle,
            wid,
            rect(50.0, 0.0, 10.0, 10.0),
            rect(40.0, 0.0, 10.0, 10.0),
        )));
        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 4, "{requests:?}");
        assert_animation_frame(&requests[0], wid, rect(50.0, 0.0, 10.0, 10.0));
        assert!(matches!(requests[1], Request::EndWindowAnimation(w) if w == wid));
        assert!(matches!(requests[2], Request::BeginWindowAnimation(w) if w == wid));
        assert_animation_pos(&requests[3], wid, CGPoint::new(40.0, 0.0));
        assert!(manager.active.is_none());
    }

    /// The frame each window ends at, applying requests the way the app thread
    /// does: EndWindowAnimation re-applies the last sized animation frame.
    fn final_frames(requests: &[Request]) -> BTreeMap<WindowId, CGRect> {
        let mut frames = BTreeMap::new();
        let mut last_sized = BTreeMap::new();
        for request in requests {
            match *request {
                Request::SetWindowFrame(wid, frame, _) => {
                    frames.insert(wid, frame);
                }
                Request::AnimationFrame { wid, frame, set_size, .. } => {
                    if set_size {
                        frames.insert(wid, frame);
                        last_sized.insert(wid, frame);
                    } else {
                        frames.entry(wid).or_insert(frame).origin = frame.origin;
                    }
                }
                Request::BeginWindowAnimation(wid) => {
                    last_sized.remove(&wid);
                }
                Request::EndWindowAnimation(wid) => {
                    if let Some(frame) = last_sized.remove(&wid) {
                        frames.insert(wid, frame);
                    }
                }
                _ => {}
            }
        }
        frames
    }

    /// Three windows get a sized frame, then position-only frames. wid1 moves
    /// until the end, wid2 parks early and wid3 stops moving before the last
    /// frame.
    fn scroll_with_sized_frames_mid_way(
        manager: &mut AnimationManager,
        handle: &AppThreadHandle,
        wids: [WindowId; 3],
    ) -> BTreeMap<WindowId, CGRect> {
        let mut last = BTreeMap::new();
        let mut frame = |wid: WindowId, x: f64, width: f64| {
            let finish = rect(x, 0.0, width, 10.0);
            last.insert(wid, finish);
            (wid, finish)
        };
        let steps: [&[(WindowId, CGRect)]; 3] = [
            &[
                frame(wids[0], 100.0, 20.0),
                frame(wids[1], 200.0, 20.0),
                frame(wids[2], 300.0, 20.0),
            ],
            &[
                frame(wids[0], 90.0, 20.0),
                frame(wids[1], -20.0, 20.0),
                frame(wids[2], 290.0, 20.0),
            ],
            &[frame(wids[0], 80.0, 20.0)],
        ];
        for (i, step) in steps.into_iter().enumerate() {
            let mut animation = Animation::new();
            for &(wid, finish) in step {
                // The size changes on the first frame only.
                let start = if i == 0 {
                    rect(0.0, 0.0, 10.0, 10.0)
                } else {
                    finish
                };
                animation.add_window(handle, wid, start, finish, false, TransactionId::default());
            }
            manager.handle_message(Message::ScrollFrame(animation));
        }
        last
    }

    #[test]
    fn scroll_end_leaves_every_window_at_its_last_frame() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wids = [
            WindowId::new(1, 1),
            WindowId::new(1, 2),
            WindowId::new(1, 3),
        ];
        let mut manager = AnimationManager::new();
        let mut expected = scroll_with_sized_frames_mid_way(&mut manager, &handle, wids);

        let end_frame = rect(70.0, 0.0, 20.0, 10.0);
        manager.handle_message(Message::ScrollEnd(animation(
            &handle,
            wids[0],
            rect(80.0, 0.0, 20.0, 10.0),
            end_frame,
        )));
        expected.insert(wids[0], end_frame);
        let requests = collect_requests(&mut rx);
        assert_eq!(final_frames(&requests), expected);
        assert!(manager.scrolling.is_empty());
    }

    #[test]
    fn empty_scroll_end_leaves_every_window_at_its_last_frame() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wids = [
            WindowId::new(1, 1),
            WindowId::new(1, 2),
            WindowId::new(1, 3),
        ];
        let mut manager = AnimationManager::new();
        let expected = scroll_with_sized_frames_mid_way(&mut manager, &handle, wids);

        manager.handle_message(Message::ScrollEnd(Animation::new()));
        let requests = collect_requests(&mut rx);
        assert_eq!(final_frames(&requests), expected);
        let ended = requests.iter().filter(|r| matches!(r, Request::EndWindowAnimation(_))).count();
        assert_eq!(ended, 3);
    }

    fn scroll_frame(handle: &AppThreadHandle, frames: &[(WindowId, CGRect, u32)]) -> Animation {
        let mut animation = Animation::new();
        for &(wid, finish, txid) in frames {
            animation.add_window(handle, wid, finish, finish, false, TransactionId(txid));
        }
        animation
    }

    /// The sized frame and txid each window gets right before its
    /// EndWindowAnimation.
    fn final_sized_frames(requests: &[Request]) -> BTreeMap<WindowId, (CGRect, TransactionId)> {
        let mut result = BTreeMap::new();
        for pair in requests.windows(2) {
            if let [
                Request::AnimationFrame {
                    wid,
                    frame,
                    set_size: true,
                    txid,
                },
                Request::EndWindowAnimation(ended),
            ] = pair
                && wid == ended
            {
                result.insert(*wid, (*frame, *txid));
            }
        }
        result
    }

    #[test]
    fn final_frame_carries_the_last_txid_of_each_window() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let (w1, w2) = (WindowId::new(1, 1), WindowId::new(1, 2));
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::ScrollFrame(scroll_frame(
            &handle,
            &[
                (w1, rect(10.0, 0.0, 10.0, 10.0), 1),
                (w2, rect(20.0, 0.0, 10.0, 10.0), 1),
            ],
        )));
        manager.handle_message(Message::ScrollFrame(scroll_frame(
            &handle,
            &[
                (w1, rect(15.0, 0.0, 10.0, 10.0), 2),
                (w2, rect(25.0, 0.0, 10.0, 10.0), 2),
            ],
        )));
        collect_requests(&mut rx);
        manager.handle_message(Message::ScrollEnd(scroll_frame(
            &handle,
            &[(w1, rect(18.0, 0.0, 10.0, 10.0), 3)],
        )));
        let finals = final_sized_frames(&collect_requests(&mut rx));
        assert_eq!(
            finals,
            BTreeMap::from([
                (w1, (rect(18.0, 0.0, 10.0, 10.0), TransactionId(3))),
                (w2, (rect(25.0, 0.0, 10.0, 10.0), TransactionId(2))),
            ])
        );
    }

    #[test]
    fn size_change_on_the_last_frame_is_the_final_frame() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut manager = AnimationManager::new();
        let mut all = vec![];
        for x in [100.0, 80.0, 70.0] {
            manager.handle_message(Message::ScrollFrame(scroll_frame(
                &handle,
                &[(wid, rect(x, 0.0, 20.0, 10.0), 1)],
            )));
            all.extend(collect_requests(&mut rx));
        }
        // The column is resized on the tick that ends the scroll.
        let resized = rect(65.0, 0.0, 40.0, 10.0);
        manager.handle_message(Message::ScrollEnd(animation(
            &handle,
            wid,
            rect(70.0, 0.0, 20.0, 10.0),
            resized,
        )));
        all.extend(collect_requests(&mut rx));
        assert_eq!(final_frames(&all), BTreeMap::from([(wid, resized)]));
        assert!(
            !all.iter().any(|r| matches!(r, Request::SetWindowFrame(..))),
            "{all:?}"
        );
    }

    #[test]
    fn second_scroll_end_in_a_row_ends_nothing_again() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::ScrollFrame(scroll_frame(
            &handle,
            &[(wid, rect(10.0, 0.0, 10.0, 10.0), 1)],
        )));
        manager.handle_message(Message::ScrollEnd(Animation::new()));
        let first = collect_requests(&mut rx);
        assert_eq!(
            first.iter().filter(|r| matches!(r, Request::EndWindowAnimation(_))).count(),
            1
        );

        manager.handle_message(Message::ScrollEnd(Animation::new()));
        assert!(collect_requests(&mut rx).is_empty());

        // The window's animation has ended, so a frame for it in another
        // ScrollEnd is a plain write.
        let frame = rect(30.0, 0.0, 10.0, 10.0);
        manager.handle_message(Message::ScrollEnd(animation(
            &handle,
            wid,
            rect(10.0, 0.0, 10.0, 10.0),
            frame,
        )));
        let second = collect_requests(&mut rx);
        assert_eq!(second.len(), 1, "{second:?}");
        assert_set_window_frame(&second[0], wid, frame);
        assert!(manager.scrolling.is_empty());
    }

    #[test]
    fn scroll_after_a_scroll_end_begins_the_window_again() {
        let (tx, mut rx) = unbounded_channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut manager = AnimationManager::new();
        let step = |manager: &mut AnimationManager, x| {
            manager.handle_message(Message::ScrollFrame(scroll_frame(
                &handle,
                &[(wid, rect(x, 0.0, 10.0, 10.0), 1)],
            )));
        };
        step(&mut manager, 10.0);
        manager.handle_message(Message::ScrollEnd(Animation::new()));
        step(&mut manager, 20.0);
        manager.handle_message(Message::ScrollEnd(Animation::new()));
        let requests = collect_requests(&mut rx);
        let kinds = requests
            .iter()
            .map(|r| match r {
                Request::BeginWindowAnimation(_) => "begin",
                Request::AnimationFrame { set_size: false, .. } => "move",
                Request::AnimationFrame { set_size: true, .. } => "sized",
                Request::EndWindowAnimation(_) => "end",
                _ => "other",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "begin", "move", "sized", "end", "begin", "move", "sized", "end"
            ]
        );
        assert_eq!(final_frames(&requests)[&wid], rect(20.0, 0.0, 10.0, 10.0));
    }
}
