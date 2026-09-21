// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Reactor's job is to maintain coherence between the system and model state.
//!
//! It takes events from the rest of the system and builds a coherent picture of
//! what is going on. It shares this with the layout actor, and reacts to layout
//! changes by sending requests out to the other actors in the system.

mod animation;
mod main_window;
mod replay;
mod system;

#[cfg(test)]
mod restore_snapshots;
#[cfg(test)]
mod testing;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{mem, thread};

use animation::{Animation, AnimationManager, Message as AnimationMessage};
use main_window::MainWindowTracker;
use objc2_core_foundation::CGRect;
use redact::Secret;
pub use replay::{Record, replay};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use system::{LiveSystem, NoSystem, SystemActions};
use tokio::sync::mpsc;
use tracing::{Span, debug, error, info, instrument, trace, warn};

use super::mouse;
use crate::actor::app::{AppInfo, AppThreadHandle, Quiet, Request, WindowId, WindowInfo, pid_t};
use crate::actor::layout::{self, LayoutCommand, LayoutEvent, LayoutManager, LayoutWindowInfo};
use crate::actor::raise::{self, RaiseManager, RaiseRequest};
use crate::actor::space_manager::SpaceManager;
use crate::actor::{group_bars, space_manager, status, window_server, wm_controller};
use crate::collections::{HashMap, HashSet};
use crate::config::Config;
use crate::log::{self, MetricsCommand};
use crate::model::scratchpad::{
    FractionalRect, PendingShowId, ScratchpadAction, ScratchpadWindowState,
};
use crate::sys::event::MouseState;
use crate::sys::executor::Executor;
use crate::sys::geometry::{CGRectDef, CGRectExt, SameAs, round_to_physical};
use crate::sys::screen::{CoordinateConverter, ScreenSpace, SpaceId};
use crate::sys::timer::Timer;
use crate::sys::window_server::{WindowServerId, WindowServerInfo, WindowsOnScreen};

pub type Sender = crate::actor::Sender<Event>;
pub type Receiver = crate::actor::Receiver<Event>;

pub fn channel() -> (Sender, Receiver) {
    crate::actor::channel()
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    /// The screen layout, including resolution, changed. This is always the
    /// first event sent on startup.
    ///
    /// The first vec is the frame for each screen. The main screen is always
    /// first in the list.
    ///
    /// See the `SpaceChanged` event for an explanation of the other parameters.
    ScreenParametersChanged {
        #[serde_as(as = "Vec<CGRectDef>")]
        frames: Vec<CGRect>,
        spaces: Vec<Option<SpaceId>>,
        scale_factors: Vec<f64>,
        converter: CoordinateConverter,
        on_screen: WindowsOnScreen,
    },

    /// The current space changed.
    ///
    /// There is one SpaceId per screen in the last ScreenParametersChanged
    /// event. `None` in the SpaceId vec disables managing windows on that
    /// screen until the next space change.
    ///
    /// WindowsOnScreen is included to avoid doing two updates in rapid
    /// succession. If there are windows we know were destroyed in the new
    /// space we'll start rearranging things, only to do so again if we
    /// discover newly added windows.
    ///
    /// TODO: In the future WindowsOnScreenUpdated should include a mapping
    /// of SpaceId to window list, and Reactor would maintain a list per
    /// space. Then we can update the windows on screen for the space before
    /// sending SpaceChanged.
    SpaceChanged(Vec<Option<SpaceId>>, WindowsOnScreen),

    /// The space each screen shows, including spaces that are not managed.
    ///
    /// Sent just before `ScreenParametersChanged` and `SpaceChanged`, in the
    /// order of their screens.
    ScreenSpacesChanged(Vec<Option<ScreenSpace>>),

    /// All running apps at launch have been registered.
    StartupComplete,

    /// An application was launched. This event is also sent for every running
    /// application on startup.
    ///
    /// Both WindowInfo (accessibility) and WindowServerInfo are collected for
    /// any already-open windows when the launch event is sent. Since this
    /// event isn't ordered with respect to the Space events, it is possible to
    /// receive this event for a space we just switched off of.. FIXME. The same
    /// is true of WindowCreated events.
    ApplicationLaunched {
        pid: pid_t,
        info: AppInfo,
        #[serde(skip, default = "replay::deserialize_app_thread_handle")]
        handle: AppThreadHandle,
        is_frontmost: bool,
        main_window: Option<WindowId>,
        visible_windows: Vec<(WindowId, WindowInfo)>,
    },
    ApplicationTerminated(pid_t),
    ApplicationThreadTerminated(pid_t),
    ApplicationActivated(pid_t, Quiet),
    ApplicationDeactivated(pid_t),
    /// The application was hidden (`true`) or shown (`false`).
    ApplicationHiddenChanged(pid_t, bool),
    ApplicationGloballyActivated(pid_t),
    ApplicationGloballyDeactivated(pid_t),
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),

    /// A scratchpad window that a launch should show is no longer expected:
    /// the launch failed or the window did not appear in time.
    ScratchpadShowExpired(PendingShowId),

    /// Moving a scratchpad window to the current space was not confirmed in
    /// time, or could not be started. The window is shown where it is.
    ScratchpadMoveEnded(SpaceMoveId),

    WindowsDiscovered {
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
    },
    WindowCreated(WindowId, WindowInfo, MouseState),

    /// Updated list of windows visible on screen from the window server.
    ///
    /// Sent after space changes, app launches, and window creation. When
    /// `pid` is set, only that app's windows changed.
    WindowsOnScreenUpdated {
        pid: Option<pid_t>,
        on_screen: WindowsOnScreen,
    },

    // TODO: Consider replacing with WindowsOnScreenUpdated.
    WindowBecameVisible(WindowId),
    WindowDestroyed(WindowId),
    WindowFrameChanged(
        WindowId,
        #[serde(with = "CGRectDef")] CGRect,
        TransactionId,
        Requested,
        Option<MouseState>,
    ),

    /// Left mouse button was released.
    ///
    /// Layout changes are suppressed while the button is down so that they
    /// don't interfere with drags. This event is used to update the layout in
    /// case updates were supressed while the button was down.
    ///
    /// FIXME: This can be interleaved incorrectly with the MouseState in app
    /// actor events.
    MouseUp,
    /// The mouse cursor moved over a new window. Only sent if focus-follows-
    /// mouse is enabled.
    ///
    /// The second field is the process the window server was routing keyboard
    /// events to when the mouse moved, if it could be read. It is read in the
    /// mouse actor so it describes the same moment as the mouse position.
    MouseMovedOverWindow(WindowServerId, #[serde(default)] Option<pid_t>),

    /// A raise request completed. Used by the raise manager to track when
    /// all raise requests in a sequence have finished.
    RaiseCompleted {
        window_id: WindowId,
        sequence_id: u64,
    },

    /// A raise request failed. None of its windows will be raised.
    ///
    /// `quiet` is the one the request was made with. A non-quiet request was
    /// meant to move the focus, so the layout has to be reconciled with the
    /// main window that is actually focused.
    RaiseRequestFailed {
        windows: Vec<WindowId>,
        sequence_id: u64,
        quiet: Quiet,
    },

    /// A raise sequence timed out. Used by the raise manager to clean up
    /// pending raises that took too long.
    RaiseTimeout {
        sequence_id: u64,
    },

    LeftMouseDown(
        #[serde(with = "crate::sys::geometry::CGPointDef")] objc2_core_foundation::CGPoint,
    ),
    LeftMouseDragged(
        #[serde(with = "crate::sys::geometry::CGPointDef")] objc2_core_foundation::CGPoint,
    ),

    ScrollWheel {
        delta_x: f64,
        delta_y: f64,
        alt_held: bool,
    },

    Command(Command),
    ConfigChanged(Arc<Config>),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Requested(pub bool);

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Command {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReactorCommand {
    Debug,
    Serialize,
    SaveAndExit,
    /// Shows or hides the scratchpad window with the given name.
    ToggleScratchpad {
        name: String,
        /// Bundle id to launch when there is no window. Without it, the
        /// command only logs a warning when there is no window.
        #[serde(default)]
        launch: Option<String>,
    },
}

pub struct Reactor {
    config: Arc<Config>,
    apps: HashMap<pid_t, AppState>,
    layout: LayoutManager,
    /// One-shot frame targets requested by layout transitions. They are merged
    /// into the next animation alongside the continuously calculated layout.
    pending_frame_overrides: HashMap<WindowId, CGRect>,
    windows: HashMap<WindowId, WindowState>,
    window_server_info: HashMap<WindowServerId, WindowServerInfo>,
    window_ids: HashMap<WindowServerId, WindowId>,
    visible_windows: HashSet<WindowServerId>,
    screens: Vec<Screen>,
    /// The space each screen shows, from the last `ScreenSpacesChanged`.
    /// Unlike `Screen::space`, this includes spaces that are not managed.
    screen_spaces: Vec<Option<ScreenSpace>>,
    active_screen_idx: Option<u16>,
    main_window_tracker: MainWindowTracker,
    in_drag: bool,
    /// The window the user is currently resizing with the mouse, if any.
    ///
    /// We don't write frames to this window until the resize ends, since a
    /// write mid-drag fights the user's mouse.
    resizing_window: Option<WindowId>,
    /// Recent attempts to write a frame to a window, used to stop fighting apps
    /// that move their windows back.
    frame_attempts: HashMap<WindowId, FrameAttempt>,
    /// Scratchpad windows being moved to the current space. Each is placed and
    /// focused once it is on screen or its move ends.
    pending_space_moves: BTreeMap<WindowId, PendingSpaceMove>,
    next_space_move_id: u64,
    record: Record,
    /// Only [`Reactor::spawn`] installs one that acts on the system, so tests
    /// and replays never do.
    system: Box<dyn SystemActions>,
    raise_manager_tx: raise::Sender,
    animation_tx: Option<animation::Sender>,
    mouse_tx: Option<mouse::Sender>,
    status_tx: Option<status::Sender>,
    group_indicators_tx: group_bars::Sender,
}

/// How many times in a row we write the same frame to a window before giving
/// up, and how long a pause resets the count.
const MAX_FRAME_ATTEMPTS: u32 = 5;
const FRAME_ATTEMPT_RESET: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct FrameAttempt {
    target: CGRect,
    count: u32,
    last: Instant,
}

#[derive(Debug)]
struct AppState {
    /// `info.is_hidden` follows [`Event::ApplicationHiddenChanged`].
    pub info: AppInfo,
    pub handle: AppThreadHandle,
}

/// Extra information about the event a layout response came from.
#[derive(Default)]
struct ResponseContext {
    /// The windows visible on screen, front to back, from a snapshot taken with
    /// the event. `None` if the event didn't come with one.
    ///
    /// Only valid for the event it arrived with: raising windows changes the
    /// order and the window server doesn't report the result.
    visible_window_order: Option<Vec<WindowServerId>>,
    /// Whether the event came from the mouse moving, in which case we don't
    /// warp the mouse to the newly focused window.
    from_mouse: bool,
}

#[derive(Copy, Clone, Debug)]
struct Screen {
    frame: CGRect,
    space: Option<SpaceId>,
    scale_factor: f64,
}

/// Identifies one move of a scratchpad window to the current space.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceMoveId(u64);

/// A scratchpad window that is shown once its move to the current space ends.
#[derive(Debug)]
struct PendingSpaceMove {
    id: SpaceMoveId,
    frame: FractionalRect,
}

/// A per-window counter that tracks the last time the reactor sent a request to
/// change the window frame.
#[derive(Default, Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionId(u32);

#[derive(Debug)]
struct WindowState {
    #[allow(unused)]
    title: Secret<String>,
    /// The last known frame of the window. Always includes the last write.
    ///
    /// This value only updates monotonically with respect to writes; in other
    /// words, we only accept reads when we know they come after the last write.
    frame_monotonic: CGRect,
    is_ax_standard: bool,
    is_resizable: bool,
    ax_role: String,
    ax_subrole: Option<String>,
    last_sent_txid: TransactionId,
    window_server_id: Option<WindowServerId>,
}

impl WindowState {
    #[must_use]
    fn next_txid(&mut self) -> TransactionId {
        self.last_sent_txid.0 += 1;
        self.last_sent_txid
    }
}

impl From<WindowInfo> for WindowState {
    fn from(info: WindowInfo) -> Self {
        WindowState {
            title: info.title,
            frame_monotonic: info.frame,
            is_ax_standard: info.is_standard,
            is_resizable: info.is_resizable,
            ax_role: info.ax_role,
            ax_subrole: info.ax_subrole,
            last_sent_txid: TransactionId::default(),
            window_server_id: info.sys_id,
        }
    }
}

impl Reactor {
    /// Spawn the reactor on a dedicated thread, co-running a `WindowServer` in
    /// the same executor. Use [`channel()`] to create reactor_tx.
    #[expect(clippy::too_many_arguments)]
    pub fn spawn(
        config: Arc<Config>,
        one_space: bool,
        layout: LayoutManager,
        record: Record,
        mouse_tx: mouse::Sender,
        status_tx: status::Sender,
        group_indicators_tx: group_bars::Sender,
        reactor_tx: Sender,
        events: Receiver,
        wm_tx: wm_controller::Sender,
        ws_tx: window_server::Sender,
        ws_rx: window_server::Receiver,
        sm_tx: space_manager::Sender,
        sm_rx: space_manager::Receiver,
        skylight_tx: window_server::SkylightSender,
    ) {
        thread::Builder::new()
            .name("reactor".to_string())
            .spawn(move || {
                let mut reactor =
                    Reactor::new(config.clone(), layout, record, group_indicators_tx.clone());
                let (delayed_tx, delayed_rx) = crate::actor::channel();
                reactor.system =
                    Box::new(LiveSystem::new(reactor_tx.clone(), ws_tx.clone(), delayed_tx));
                let delayed_events = system::run_delayed_events(delayed_rx, reactor_tx.clone());
                reactor.mouse_tx.replace(mouse_tx.clone());
                reactor.status_tx.replace(status_tx.clone());
                let space_manager = SpaceManager::new(
                    one_space,
                    config,
                    reactor_tx.clone(),
                    ws_tx.clone(),
                    wm_tx.clone(),
                    status_tx,
                    group_indicators_tx,
                    mouse_tx,
                );
                let window_server = window_server::WindowServer::new(sm_tx, wm_tx, skylight_tx);
                Executor::run(async move {
                    tokio::join!(
                        reactor.run(events, reactor_tx),
                        space_manager.run(sm_rx),
                        window_server.run(ws_rx),
                        delayed_events,
                    );
                });
            })
            .unwrap();
    }

    pub fn new(
        config: Arc<Config>,
        mut layout: LayoutManager,
        mut record: Record,
        group_indicators_tx: group_bars::Sender,
    ) -> Reactor {
        // FIXME: Remove apps that are no longer running from restored state.
        record.start(&config, &layout);
        layout.set_config(&config);
        let (raise_manager_tx, _rx) = mpsc::unbounded_channel();
        Reactor {
            config,
            apps: HashMap::default(),
            layout,
            pending_frame_overrides: HashMap::default(),
            windows: HashMap::default(),
            window_ids: HashMap::default(),
            window_server_info: HashMap::default(),
            visible_windows: HashSet::default(),
            screens: vec![],
            screen_spaces: vec![],
            active_screen_idx: None,
            main_window_tracker: MainWindowTracker::default(),
            in_drag: false,
            resizing_window: None,
            frame_attempts: HashMap::default(),
            pending_space_moves: BTreeMap::new(),
            next_space_move_id: 0,
            record,
            system: Box::new(NoSystem),
            raise_manager_tx,
            animation_tx: None,
            mouse_tx: None,
            status_tx: None,
            group_indicators_tx: group_indicators_tx,
        }
    }

    pub async fn run(mut self, events: Receiver, events_tx: Sender) {
        let (raise_manager_tx, raise_manager_rx) = mpsc::unbounded_channel();
        self.raise_manager_tx = raise_manager_tx.clone();
        let (animation_tx, animation_rx) = mpsc::unbounded_channel();
        self.animation_tx = Some(animation_tx);

        let mouse_tx = self.mouse_tx.clone();
        let reactor_task = self.run_reactor_loop(events);
        let raise_manager_task = RaiseManager::run(raise_manager_rx, events_tx, mouse_tx);
        let animation_task = AnimationManager::run(animation_rx);

        let _ = tokio::join!(reactor_task, raise_manager_task, animation_task);
    }

    async fn run_reactor_loop(mut self, mut events: Receiver) {
        // TODO: Accessibility APIs may be too slow for 120Hz; consider screen-capture animation approach.
        let tick_interval = Duration::from_secs_f64(1.0 / 120.0);
        let mut tick_timer = Timer::manual();

        loop {
            let animating = self.layout.has_active_scroll_animation();
            tokio::select! {
                event = events.recv() => {
                    let Some((span, event)) = event else { break };
                    let _guard = span.enter();
                    let was_animating = self.layout.has_active_scroll_animation();
                    self.handle_event(event);
                    if !was_animating && self.layout.has_active_scroll_animation() {
                        tick_timer.set_next_fire(Duration::ZERO);
                    }
                }
                _ = tick_timer.next(), if animating => {
                    self.layout.tick_viewports();
                    self.update_layout(&[], true);
                    if self.layout.has_active_scroll_animation() {
                        tick_timer.set_next_fire(tick_interval);
                    }
                }
            }
        }
    }

    fn log_event(&self, event: &Event) {
        match event {
            // Record more noisy events as trace logs instead of debug.
            Event::WindowFrameChanged(..)
            | Event::MouseUp
            | Event::LeftMouseDown(_)
            | Event::LeftMouseDragged(_) => trace!(?event, "Event"),
            _ => debug!(?event, "Event"),
        }
    }

    fn handle_event(&mut self, event: Event) {
        self.record.on_event(&event);
        self.log_event(&event);
        let mut animation_focus_wids: Vec<WindowId> = Vec::new();
        let mut is_resize = false;
        let raised_window = self.main_window_tracker.handle_event(&event);
        match event {
            Event::ApplicationLaunched {
                pid,
                info,
                handle,
                visible_windows,
                is_frontmost: _,
                main_window: _,
            } => {
                self.apps.insert(pid, AppState { info, handle });
                self.on_windows_discovered(pid, visible_windows, vec![]);
            }
            Event::StartupComplete => {
                self.send_layout_event(LayoutEvent::AppsRunningUpdated(
                    self.apps.keys().copied().collect(),
                ));
            }
            Event::ApplicationTerminated(pid) => {
                if let Some(app) = self.apps.get_mut(&pid) {
                    _ = app.handle.send(Request::Terminate);
                }
            }
            Event::ApplicationThreadTerminated(pid) => {
                if let Some(app) = self.apps.remove(&pid)
                    && let Some(bundle_id) = &app.info.bundle_id
                {
                    self.layout.cancel_scratchpad_shows_for_app(bundle_id);
                }
                self.pending_space_moves.retain(|wid, _| wid.pid != pid);
                self.send_layout_event(LayoutEvent::AppClosed(pid));
            }
            Event::ScratchpadMoveEnded(id) => {
                if let Some((&wid, _)) = self.pending_space_moves.iter().find(|(_, m)| m.id == id) {
                    let moved = self.pending_space_moves.remove(&wid).unwrap();
                    debug!(?wid, "Showing scratchpad window without waiting for its move");
                    self.place_scratchpad(wid, moved.frame);
                }
            }
            Event::ScratchpadShowExpired(id) => {
                if let Some(name) = self.layout.cancel_scratchpad_show(id) {
                    info!(
                        name,
                        "Scratchpad window was not shown: it did not appear in time"
                    );
                }
            }
            Event::ScreenSpacesChanged(spaces) => self.screen_spaces = spaces,
            Event::ApplicationHiddenChanged(pid, hidden) => {
                if let Some(app) = self.apps.get_mut(&pid) {
                    // Only a shown app becoming hidden cancels its moves; the
                    // unhide sent by a show is reported before any later hide.
                    if hidden && !app.info.is_hidden {
                        self.pending_space_moves.retain(|wid, _| {
                            let keep = wid.pid != pid;
                            if !keep {
                                debug!(?wid, "Not showing scratchpad window: its app was hidden");
                            }
                            keep
                        });
                    }
                    app.info.is_hidden = hidden;
                }
            }
            Event::ApplicationActivated(..)
            | Event::ApplicationDeactivated(..)
            | Event::ApplicationGloballyActivated(..)
            | Event::ApplicationGloballyDeactivated(..)
            | Event::ApplicationMainWindowChanged(..) => {
                // Handled by MainWindowTracker.
            }
            Event::WindowsDiscovered { pid, new, known_visible } => {
                self.on_windows_discovered(pid, new, known_visible);
            }
            Event::WindowCreated(wid, window, mouse_state) => {
                // TODO: It's possible for a window to be on multiple spaces
                // or move spaces. (Add a test)
                // FIXME: We assume all windows are on the main screen.
                if let Some(wsid) = window.sys_id {
                    self.window_ids.insert(wsid, wid);
                }
                self.windows.insert(wid, window.clone().into());
                if mouse_state == MouseState::Down {
                    self.in_drag = true;
                    // Suppress updates while left button is pressed in case
                    // a drag is in progress.
                }
            }
            Event::WindowsOnScreenUpdated { pid, on_screen } => {
                match pid {
                    Some(_) => self.update_partial_window_server_info(on_screen),
                    None => self.update_complete_window_server_info(on_screen),
                }
                self.place_moved_scratchpads();
            }
            Event::WindowBecameVisible(wid) => {
                if self.window_is_tracked(wid)
                    && let Some(window) = self.windows.get(&wid)
                    && let Some(info) = self.layout_window_info(wid)
                {
                    match self.best_space_for_window(&window.frame_monotonic) {
                        Some(space) => {
                            animation_focus_wids.push(wid);
                            self.send_layout_event(LayoutEvent::WindowAdded(space, wid, info));
                        }
                        None => self.send_layout_event(LayoutEvent::ScratchpadCandidates(vec![(
                            wid, info,
                        )])),
                    }
                }
            }
            Event::WindowDestroyed(wid) => {
                self.layout.cancel_interactive_state();
                self.in_drag = false;
                self.resizing_window = None;
                if self.windows.remove(&wid).is_none() {
                    warn!("Got destroyed event for unknown window {wid:?}");
                }
                self.pending_space_moves.remove(&wid);
                self.frame_attempts.remove(&wid);
                //animation_focus_wid = self.window_order.last().cloned();
                self.send_layout_event(LayoutEvent::WindowRemoved(wid));
            }
            Event::WindowFrameChanged(wid, new_frame, last_seen, requested, mouse_state) => {
                if mouse_state == Some(MouseState::Up) {
                    // The button is up, so any resize we were holding off on is
                    // over, even if we never saw the MouseUp event.
                    self.resizing_window = None;
                }
                let window = self.windows.get_mut(&wid).unwrap();
                if last_seen != window.last_sent_txid {
                    // Ignore events that happened before the last time we
                    // changed the size or position of this window. Otherwise
                    // we would update the layout model incorrectly.
                    debug!(?last_seen, ?window.last_sent_txid, "Ignoring resize");
                    return;
                }
                if requested.0 {
                    // TODO: If the size is different from requested, applying a
                    // correction to the model can result in weird feedback
                    // loops, so we ignore these for now.
                    return;
                }
                let old_frame = mem::replace(&mut window.frame_monotonic, new_frame);
                if old_frame == new_frame {
                    return;
                }
                self.send_layout_event(LayoutEvent::WindowFrameChanged { wid, frame: new_frame });
                let old_screen = self.best_screen_idx_for_window(&old_frame);
                let new_screen = self.best_screen_idx_for_window(&new_frame);
                if let Some(old) = old_screen
                    && let Some(new) = new_screen
                    && old != new
                    && let Some(info) = self.layout_window_info(wid)
                {
                    self.send_layout_event(LayoutEvent::WindowSpaceChanged {
                        wid,
                        added: self.screens[new].space,
                        removed: self.screens[old].space,
                        info,
                    });
                }
                if old_frame.size != new_frame.size {
                    let screens = self
                        .screens
                        .iter()
                        .flat_map(|screen| Some((screen.space?, screen.frame)))
                        .collect::<Vec<_>>();
                    // This event is ignored if the window is not in the layout.
                    self.send_layout_event(LayoutEvent::WindowResized {
                        wid,
                        old_frame,
                        new_frame,
                        screens,
                    });
                    if mouse_state == Some(MouseState::Down) {
                        self.resizing_window = Some(wid);
                    }
                    is_resize = true;
                } else if mouse_state == Some(MouseState::Down) {
                    self.in_drag = true;
                }
            }
            Event::ScreenParametersChanged {
                frames,
                spaces,
                converter,
                scale_factors,
                on_screen,
            } => {
                info!("screen parameters changed");
                let visible_window_order = on_screen.visible.clone();
                self.update_complete_window_server_info(on_screen);
                self.screens = frames
                    .into_iter()
                    .zip(spaces.clone())
                    .zip(scale_factors)
                    .map(|((frame, space), scale_factor)| Screen { frame, space, scale_factor })
                    .collect();
                let response = self
                    .screens
                    .iter()
                    .filter_map(|screen| screen.space.map(|space| (space, screen.frame.size)))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|(space, size)| {
                        self.layout.handle_event(LayoutEvent::SpaceExposed(space, size))
                    })
                    .reduce(layout::EventResponse::coalesce);
                if let Some(response) = response {
                    self.handle_layout_response_with_context(
                        response,
                        ResponseContext {
                            visible_window_order: Some(visible_window_order),
                            ..Default::default()
                        },
                    );
                    for space in self.screens.iter().flat_map(|screen| screen.space) {
                        self.layout.debug_tree_desc(space, "after event", false);
                    }
                }
                self.update_active_screen();
                self.place_moved_scratchpads();
                // FIXME: Update visible windows if space changed.
                // Forward the event to group_indicators. We serialize these
                // through the reactor instead of delivering directly from
                // wm_controller in order to eliminate possible races with other
                // events sent by the reactor.
                self.group_indicators_tx
                    .send(group_bars::Event::ScreenParametersChanged(spaces, converter));
            }
            Event::SpaceChanged(spaces, on_screen) => {
                let visible_window_order = on_screen.visible.clone();
                self.update_complete_window_server_info(on_screen);
                if spaces.len() != self.screens.len() {
                    warn!(
                        "Ignoring space change event: we have {} spaces, but {} screens",
                        spaces.len(),
                        self.screens.len()
                    );
                    return;
                }
                self.layout.cancel_interactive_state();
                self.in_drag = false;
                self.resizing_window = None;
                info!("space changed");
                for (space, screen) in spaces.iter().zip(&mut self.screens) {
                    screen.space = *space;
                }
                let response = self
                    .screens
                    .iter()
                    .filter_map(|screen| screen.space.map(|space| (space, screen.frame.size)))
                    .map(|(space, size)| {
                        self.layout.handle_event(LayoutEvent::SpaceExposed(space, size))
                    })
                    .reduce(layout::EventResponse::coalesce);
                if let Some(response) = response {
                    self.handle_layout_response_with_context(
                        response,
                        ResponseContext {
                            visible_window_order: Some(visible_window_order),
                            ..Default::default()
                        },
                    );
                    for space in self.screens.iter().flat_map(|screen| screen.space) {
                        self.layout.debug_tree_desc(space, "after event", false);
                    }
                }
                if let Some(main_window) = self.main_window() {
                    let spaces = spaces.iter().copied().flatten().collect();
                    self.send_layout_event(LayoutEvent::WindowFocused(spaces, main_window));
                }
                self.update_active_screen();
                self.place_moved_scratchpads();
                self.update_visible_windows();
            }
            Event::LeftMouseDown(point) => {
                if let Some(screen) = self.active_screen()
                    && let Some(space) = screen.space
                {
                    if let Some((col, win, edges)) =
                        self.layout.hit_test_scroll_edges(space, point, screen.frame, &self.config)
                    {
                        self.layout.begin_interactive_resize(col, win, edges, point);
                        self.in_drag = true;
                    } else if let Some((wid, node)) =
                        self.layout.hit_test_scroll_window(space, point, screen.frame, &self.config)
                    {
                        self.layout.begin_interactive_move(space, wid, node, point);
                        self.in_drag = true;
                    }
                }
            }
            Event::LeftMouseDragged(point) => {
                if let Some(&screen) = self.active_screen() {
                    if screen.space.is_some() {
                        if self.layout.update_interactive_resize(point, screen.frame) {
                            self.update_layout(&[], true);
                        } else if self.layout.update_interactive_move(
                            point,
                            screen.frame,
                            &self.config,
                        ) {
                            self.update_layout(&[], false);
                        }
                    }
                }
            }
            Event::MouseUp => {
                if self.layout.has_interactive_state() {
                    if let Some(&screen) = self.active_screen() {
                        if let Some(space) = screen.space {
                            self.layout.end_interactive_resize(space, screen.frame, &self.config);
                            self.layout.end_interactive_move(space, screen.frame, &self.config);
                        }
                    }
                }
                self.in_drag = false;
                self.resizing_window = None;
                // Now re-check the layout.
            }
            Event::MouseMovedOverWindow(wsid, key_focus_pid) => {
                let Some(&wid) = self.window_ids.get(&wsid) else { return };
                let Some(window) = self.windows.get(&wid) else { return };
                let Some(to_space) = self.best_space_for_window(&window.frame_monotonic) else {
                    // The space is disabled.
                    return;
                };
                let current_main = match (self.main_window_space(), self.main_window()) {
                    (Some(space), Some(id)) => Some((space, id)),
                    _ => None,
                };
                // Spotlight and similar apps can take keyboard focus without
                // activating, which leaves the main window unchanged. We don't
                // manage these windows and should avoid stealing focus from them.
                if let Some(key_focus_pid) = key_focus_pid
                    && current_main.is_some_and(|(_, main)| main.pid != key_focus_pid)
                {
                    debug!(
                        ?key_focus_pid,
                        ?current_main,
                        "Ignoring mouse move; keyboard focus is elsewhere"
                    );
                    return;
                }
                self.send_layout_event_with_context(
                    LayoutEvent::MouseMovedOverWindow {
                        over: (to_space, wid),
                        // TODO: Track focused window and use it here rather
                        // than main, to avoid stealing focus from a panel (of
                        // the main app; otherwise we would have returned above).
                        current_main,
                    },
                    ResponseContext {
                        from_mouse: true,
                        ..Default::default()
                    },
                );
            }
            Event::RaiseCompleted { window_id, sequence_id } => {
                let msg = raise::Event::RaiseCompleted { window_id, sequence_id };
                _ = self.raise_manager_tx.send((Span::current(), msg));
            }
            Event::RaiseRequestFailed { windows, sequence_id, quiet } => {
                let msg = raise::Event::RaiseRequestFailed { windows, sequence_id };
                _ = self.raise_manager_tx.send((Span::current(), msg));
                if quiet == Quiet::No
                    && let Some(main_window) = self.main_window()
                {
                    // The raise didn't move the focus, so bring the layout
                    // selection back to the window that has it.
                    let spaces = self.screens.iter().flat_map(|screen| screen.space).collect();
                    self.send_layout_event(LayoutEvent::WindowFocused(spaces, main_window));
                }
            }
            Event::RaiseTimeout { sequence_id } => {
                let msg = raise::Event::RaiseTimeout { sequence_id };
                _ = self.raise_manager_tx.send((Span::current(), msg));
            }
            Event::ScrollWheel { delta_x, delta_y, alt_held } => {
                if !self.config.settings.experimental.scroll.enable {
                    return;
                }
                // TODO: Make the modifier key configurable.
                if !alt_held {
                    return;
                }
                if let Some(&screen) = self.active_screen() {
                    if let Some(space) = screen.space {
                        let scroll_config = &self.config.settings.experimental.scroll;
                        let delta = if delta_x != 0.0 { delta_x } else { delta_y };
                        let response = self.layout.handle_scroll_wheel(
                            space,
                            delta,
                            &screen.frame,
                            scroll_config,
                        );
                        self.handle_layout_response(response);
                    }
                }
            }
            Event::Command(Command::Layout(cmd)) => {
                info!(?cmd);
                let visible_spaces =
                    self.screens.iter().flat_map(|screen| screen.space).collect::<Vec<_>>();
                // macOS can temporarily have no main window (for example after
                // clicking the desktop). Keep keyboard layout commands usable
                // by targeting the last active screen in that case.
                let command_space = self
                    .main_window_space()
                    .or_else(|| self.active_screen().and_then(|screen| screen.space));
                let response = self.layout.handle_command(command_space, &visible_spaces, cmd);
                self.handle_layout_response(response);
            }
            Event::Command(Command::Metrics(cmd)) => log::handle_command(cmd),
            Event::Command(Command::Reactor(ReactorCommand::Debug)) => {
                for screen in &self.screens {
                    if let Some(space) = screen.space {
                        self.layout.debug_tree_desc(space, "", true);
                    }
                }
            }
            Event::Command(Command::Reactor(ReactorCommand::Serialize)) => {
                println!("{}", self.layout.serialize_to_string());
            }
            Event::Command(Command::Reactor(ReactorCommand::SaveAndExit)) => {
                info!("SaveAndExit command received");
                match self.layout.save(crate::config::restore_file()) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        error!("Could not save layout: {e}");
                        std::process::exit(3);
                    }
                }
            }
            Event::Command(Command::Reactor(ReactorCommand::ToggleScratchpad { name, launch })) => {
                self.toggle_scratchpad(&name, launch.as_deref());
            }
            Event::ConfigChanged(config) => {
                self.layout.set_config(&config);
                self.config = config;
            }
        }
        if let Some(raised_window) = raised_window {
            let spaces = self.screens.iter().flat_map(|screen| screen.space).collect();
            self.send_layout_event(LayoutEvent::WindowFocused(spaces, raised_window));
            self.update_active_screen();
        }
        if !self.in_drag {
            self.update_layout(&animation_focus_wids, is_resize);
        }
    }

    fn update_complete_window_server_info(&mut self, on_screen: WindowsOnScreen) {
        for info in on_screen.info.iter().filter(|i| i.layer == 0) {
            let Some(wid) = self.window_ids.get(&info.id) else {
                continue;
            };
            let Some(window) = self.windows.get_mut(wid) else {
                continue;
            };
            // Assume this update comes from after the last write. Typically the
            // window is on a different space than the one we're coming from
            // (unless it's on all spaces).
            //
            // TODO: It is still possible to have a race if we issued resizes on
            // this window that haven't completed yet (e.g. from an earlier
            // animation and a slow app). Consider having the app actor give us
            // updated locations on GetVisibleWindows instead.
            window.frame_monotonic = info.frame;
        }
        self.update_partial_window_server_info(on_screen);
    }

    fn update_partial_window_server_info(&mut self, on_screen: WindowsOnScreen) {
        // The on_screen snapshot always contains the complete list of visible
        // windows, even for partial (per-app) updates. Replace rather than
        // extend to avoid accumulating stale entries.
        self.visible_windows.clear();
        self.visible_windows.extend(on_screen.visible);
        self.window_server_info
            .extend(on_screen.info.into_iter().map(|info| (info.id, info)));
    }

    fn should_compare_visible_window(&self, wsid: WindowServerId) -> bool {
        let Some(info) = self.window_server_info.get(&wsid) else {
            return false;
        };
        if info.layer != 0 {
            // TODO: Revisit this if LayoutManager starts managing floating windows.
            return false;
        }
        self.screens
            .iter()
            .filter(|screen| screen.space.is_some())
            .map(|screen| screen.frame.intersection(&info.frame).size)
            .any(|size| size.width > 0.0 && size.height > 0.0)
    }

    fn update_visible_windows(&mut self) {
        // TODO: Do this correctly/more optimally using CGWindowListCopyWindowInfo
        // (see notes for on_windows_discovered below).
        for app in self.apps.values_mut() {
            // Errors mean the app terminated (and a termination event
            // is coming); ignore.
            _ = app.handle.send(Request::GetVisibleWindows);
        }
    }

    fn on_windows_discovered(
        &mut self,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        _known_visible: Vec<WindowId>,
    ) {
        // Note that we rely on the window server info, not accessibility, to
        // tell us which windows are visible.
        //
        // The accessibility APIs report that there are no visible windows when
        // at a login screen, for instance, but there is not a corresponding
        // system notification to use as context. Even if there were, lining
        // them up with the responses we get from the app would be unreliable.
        //
        // We therefore do not let accessibility `.windows()` results remove
        // known windows from the visible list. Doing so incorrectly would cause
        // us to destroy the layout. We do wait for windows to become initially
        // known to accesibility before adding them to the layout, but that is
        // not generally problematic.
        //
        // TODO: Notice when returning from the login screen and ask again for
        // undiscovered windows.
        self.window_ids
            .extend(new.iter().flat_map(|(wid, info)| info.sys_id.map(|wsid| (wsid, *wid))));
        self.windows.extend(new.into_iter().map(|(wid, info)| (wid, info.into())));
        let mut app_windows: BTreeMap<SpaceId, Vec<(WindowId, LayoutWindowInfo)>> = BTreeMap::new();
        // The layout takes the first matching window as a scratchpad: prefer
        // the main window, then visible windows, wherever they are.
        let main_window = self.main_window_tracker.app_main_window(pid);
        let mut candidates = self
            .windows
            .keys()
            .copied()
            .filter(|wid| wid.pid == pid && self.window_is_tracked(*wid))
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|wid| {
            (Some(*wid) != main_window, !self.window_is_visible(*wid), *wid)
        });
        let candidates = candidates
            .into_iter()
            .filter_map(|wid| Some((wid, self.layout_window_info(wid)?)))
            .collect();
        self.send_layout_event(LayoutEvent::ScratchpadCandidates(candidates));

        let wids = self
            .visible_windows
            .iter()
            .flat_map(|wsid| self.window_ids.get(wsid).copied())
            .filter(|wid| wid.pid == pid)
            .filter(|wid| self.window_is_tracked(*wid))
            .collect::<BTreeSet<_>>();
        for wid in wids {
            let Some(window) = self.windows.get(&wid) else { continue };
            let Some(space) = self.best_space_for_window(&window.frame_monotonic) else {
                continue;
            };
            let Some(layout_info) = self.layout_window_info(wid) else {
                continue;
            };
            app_windows.entry(space).or_default().push((wid, layout_info));
        }
        let screens = self.screens.clone();
        for screen in screens {
            let Some(space) = screen.space else { continue };
            self.send_layout_event(LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                app_windows.remove(&space).unwrap_or_default(),
            ));
        }
        // If it's possible we just added the main window to the layout, make
        // sure the layout knows it's focused.
        if let Some(main_window) = self.main_window() {
            if main_window.pid == pid {
                let spaces = self.screens.iter().flat_map(|screen| screen.space).collect();
                self.send_layout_event(LayoutEvent::WindowFocused(spaces, main_window));
            }
        }
    }

    /// The screen the window overlaps the most, or None if it is not on any
    /// screen. Apps park windows far off screen to hide them, and those belong
    /// to no screen at all.
    fn best_screen_idx_for_window(&self, frame: &CGRect) -> Option<usize> {
        self.screens
            .iter()
            .enumerate()
            .map(|(idx, screen)| (idx, screen.frame.intersection(frame).area()))
            .filter(|&(_, area)| area > 0.0)
            .max_by_key(|&(_, area)| area as i64)
            .map(|(idx, _)| idx)
            // A window with no area intersects nothing, so place it by its midpoint.
            .or_else(|| self.screens.iter().position(|screen| screen.frame.contains(frame.mid())))
    }

    fn best_space_for_window(&self, frame: &CGRect) -> Option<SpaceId> {
        self.screens[self.best_screen_idx_for_window(frame)?].space
    }

    /// Gathers the window properties the layout uses to classify a window.
    fn layout_window_info(&self, wid: WindowId) -> Option<LayoutWindowInfo> {
        let window = self.windows.get(&wid)?;
        let app = self.apps.get(&wid.pid);
        Some(LayoutWindowInfo {
            frame: window.frame_monotonic,
            bundle_id: app.and_then(|a| a.info.bundle_id.clone()),
            app_name: app.and_then(|a| a.info.localized_name.clone()),
            title: window.title.clone().into(),
            layer: window
                .window_server_id
                .and_then(|wsid| self.window_server_info.get(&wsid))
                .map(|info| info.layer),
            is_standard: window.is_ax_standard,
            is_resizable: window.is_resizable,
            ax_role: window.ax_role.clone(),
            ax_subrole: window.ax_subrole.clone(),
        })
    }

    fn update_active_screen(&mut self) {
        let changed = (|| {
            let frame = self.windows.get(&self.main_window()?)?.frame_monotonic;
            let screen = self.best_screen_idx_for_window(&frame)?;
            Some(self.active_screen_idx.replace(screen as u16) != Some(screen as u16))
        })();
        if changed.unwrap_or(false)
            && let Some(status_tx) = &mut self.status_tx
        {
            status_tx.send(status::Event::FocusedScreenChanged);
        }
    }

    fn active_screen(&self) -> Option<&Screen> {
        self.screens.get(self.active_screen_idx.unwrap_or(0) as usize)
    }

    /// Whether the window server reports the window on screen. Windows without
    /// a window server id count as visible.
    fn window_is_visible(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|window| window.window_server_id)
            .is_none_or(|wsid| self.visible_windows.contains(&wsid))
    }

    fn window_is_tracked(&self, _id: WindowId) -> bool {
        // For now we track all windows in the reactor and let the LayoutManager
        // decide what to keep.
        true
    }

    fn send_layout_event(&mut self, event: LayoutEvent) {
        self.send_layout_event_with_context(event, ResponseContext::default());
    }

    fn send_layout_event_with_context(&mut self, event: LayoutEvent, context: ResponseContext) {
        let response = self.layout.handle_event(event);
        self.handle_layout_response_with_context(response, context);
        for space in self.screens.iter().flat_map(|screen| screen.space) {
            self.layout.debug_tree_desc(space, "after event", false);
        }
    }

    fn handle_layout_response(&mut self, response: layout::EventResponse) {
        self.handle_layout_response_with_context(response, ResponseContext::default());
    }

    /// Every response from [`LayoutManager::handle_event`] goes through here,
    /// which is where the scratchpads it wants shown are taken.
    fn handle_layout_response_with_context(
        &mut self,
        response: layout::EventResponse,
        context: ResponseContext,
    ) {
        let to_show =
            std::iter::from_fn(|| self.layout.take_scratchpad_to_show()).collect::<Vec<_>>();
        self.apply_layout_response(response, context);
        for (wid, frame) in to_show {
            self.show_scratchpad(wid, frame);
        }
    }

    fn apply_layout_response(
        &mut self,
        mut response: layout::EventResponse,
        ResponseContext {
            visible_window_order,
            from_mouse,
        }: ResponseContext,
    ) {
        if let Some(visible_window_order) = visible_window_order {
            response = self.filter_response(response, &visible_window_order);
        }

        let layout::EventResponse {
            frame_overrides,
            raise_windows,
            focus_window,
        } = response;
        self.pending_frame_overrides.extend(frame_overrides);
        if raise_windows.is_empty() && focus_window.is_none() {
            return;
        }

        let mut app_handles = HashMap::default();
        for &wid in raise_windows.iter().chain(&focus_window) {
            if let Some(app) = self.apps.get(&wid.pid) {
                app_handles.insert(wid.pid, app.handle.clone());
            }
        }

        let mut windows_by_app_and_screen = HashMap::default();
        for &wid in &raise_windows {
            let Some(window) = self.windows.get(&wid) else { continue };
            windows_by_app_and_screen
                .entry((wid.pid, self.best_space_for_window(&window.frame_monotonic)))
                .or_insert(vec![])
                .push(wid);
        }

        let focus_window_with_warp = focus_window.map(|wid| {
            let warp = if self.config.settings.mouse_follows_focus && !from_mouse {
                self.windows.get(&wid).map(|w| w.frame_monotonic.mid())
            } else {
                // We disable warp above if the event itself is caused by mouse
                // movement.
                None
            };
            (wid, warp)
        });

        let msg = raise::Event::RaiseRequest(RaiseRequest {
            raise_windows: windows_by_app_and_screen.into_values().collect(),
            focus_window: focus_window_with_warp,
            app_handles,
        });

        _ = self.raise_manager_tx.send((Span::current(), msg));
    }

    fn filter_response(
        &self,
        mut response: layout::EventResponse,
        mut visible_window_order: &[WindowServerId],
    ) -> layout::EventResponse {
        if let Some(focus) = response.focus_window {
            // Attempt to match out the focus window with the top visible window.
            if let Some(&first) = visible_window_order.first()
                && focus.wsid() == Some(first)
            {
                // Note that we keep the focus window in the request, unless
                // there are no raise windows.
                visible_window_order = &visible_window_order[1..];
            } else {
                return response;
            }
        };
        let desired_visible_wsids = response
            .raise_windows
            .iter()
            .flat_map(|wid| self.windows.get(wid).and_then(|window| window.window_server_id))
            .collect::<HashSet<_>>();
        let current_top_wsids = visible_window_order
            .iter()
            .copied()
            // Filter out off-screen windows and windows on non-zero layers.
            .filter(|wsid| self.should_compare_visible_window(*wsid))
            .take(desired_visible_wsids.len())
            .collect::<HashSet<_>>();

        // Optimize the case where the response is a no-op.
        if current_top_wsids == desired_visible_wsids {
            response.focus_window.take();
            response.raise_windows.clear();
        }
        response
    }

    fn toggle_scratchpad(&mut self, name: &str, launch: Option<&str>) {
        let state =
            self.layout.scratchpad_window(name).map(|wid| self.scratchpad_window_state(wid));
        let action = self.layout.toggle_scratchpad(name, launch, |wid| {
            state.unwrap_or_else(|| {
                error!(?wid, name, "No state for the scratchpad window; showing it");
                ScratchpadWindowState {
                    app_hidden: true,
                    focused: false,
                    on_active_space: false,
                }
            })
        });
        info!(name, ?state, ?action, "Toggling scratchpad");
        match action {
            ScratchpadAction::Launch => {
                let (Some(bundle_id), Some(id)) =
                    (launch, self.layout.pending_scratchpad_show(name))
                else {
                    warn!(name, "Scratchpad has no window and no `launch` to open one");
                    return;
                };
                if let Err(err) = self.system.launch_app(name, bundle_id, id) {
                    error!(name, bundle_id, "Could not launch scratchpad app: {err}");
                    // A replay does not launch, so it needs the outcome in the
                    // trace to make the same decision.
                    self.record.on_event(&Event::ScratchpadShowExpired(id));
                    self.layout.cancel_scratchpad_show(id);
                }
            }
            ScratchpadAction::Show(wid) => {
                let frame = self.layout.scratchpad_frame(wid).unwrap_or(FractionalRect::DEFAULT);
                self.show_scratchpad(wid, frame);
            }
            ScratchpadAction::Hide(wid) => self.set_app_hidden(wid.pid, true),
        }
    }

    fn scratchpad_window_state(&self, wid: WindowId) -> ScratchpadWindowState {
        let window = self.windows.get(&wid);
        let on_screen = self.window_is_visible(wid);
        let on_active_screen = window
            .and_then(|window| self.best_screen_idx_for_window(&window.frame_monotonic))
            .is_some_and(|idx| idx == self.active_screen_idx.unwrap_or(0) as usize);
        ScratchpadWindowState {
            app_hidden: self.is_app_hidden(wid.pid),
            focused: self.main_window() == Some(wid),
            on_active_space: on_screen && on_active_screen,
        }
    }

    /// Unhides the app if needed, places the window on the active screen and
    /// focuses it. Does not wait for the app to report that it is shown.
    fn show_scratchpad(&mut self, wid: WindowId, frame: FractionalRect) {
        if self.is_app_hidden(wid.pid) {
            self.set_app_hidden(wid.pid, false);
        }
        if !self.window_is_visible(wid)
            && let Some(app) = self.apps.get(&wid.pid)
        {
            _ = app.handle.send(Request::Unminimize(wid));
        }
        self.pending_space_moves.remove(&wid);
        if let Some((wsid, space)) = self.space_move_target(wid) {
            let id = SpaceMoveId(self.next_space_move_id);
            self.next_space_move_id += 1;
            match self.system.move_window_to_space(wid, wsid, space, id) {
                Ok(()) => {
                    debug!(
                        ?wid,
                        ?space,
                        ?id,
                        "Moving scratchpad window to the current space"
                    );
                    self.pending_space_moves.insert(wid, PendingSpaceMove { id, frame });
                    return;
                }
                Err(err) => {
                    debug!(?wid, "Not moving scratchpad window: {err}");
                    // A replay does not try the move, so it needs the outcome
                    // in the trace to make the same decision.
                    self.record.on_event(&Event::ScratchpadMoveEnded(id));
                }
            }
        }
        self.place_scratchpad(wid, frame);
    }

    /// The window server id of the window and the space to move it to, if it
    /// may be on another space. A hidden app's windows are not on screen
    /// wherever they are, so they are moved too; moving a window to its own
    /// space does nothing.
    fn space_move_target(&self, wid: WindowId) -> Option<(WindowServerId, SpaceId)> {
        let wsid = self.windows.get(&wid)?.window_server_id?;
        if self.visible_windows.contains(&wsid) && !self.is_app_hidden(wid.pid) {
            return None;
        }
        let screen = self.active_screen_idx.unwrap_or(0) as usize;
        let space = (*self.screen_spaces.get(screen)?)?;
        if space.fullscreen {
            debug!(
                ?wid,
                ?space,
                "Not moving scratchpad window to a fullscreen space"
            );
            return None;
        }
        Some((wsid, space.id))
    }

    /// Places and focuses the moved scratchpad windows that are on screen now.
    fn place_moved_scratchpads(&mut self) {
        let arrived: Vec<WindowId> = self
            .pending_space_moves
            .keys()
            .copied()
            .filter(|&wid| self.window_is_visible(wid))
            .collect();
        for wid in arrived {
            let moved = self.pending_space_moves.remove(&wid).unwrap();
            self.place_scratchpad(wid, moved.frame);
        }
    }

    /// Places the window on the active screen and focuses it.
    fn place_scratchpad(&mut self, wid: WindowId, frame: FractionalRect) {
        let frame_overrides = self
            .active_screen()
            .map(|screen| vec![(wid, frame.to_frame(screen.frame))])
            .unwrap_or_default();
        self.handle_layout_response(layout::EventResponse {
            frame_overrides,
            raise_windows: vec![],
            focus_window: Some(wid),
        });
    }

    fn set_app_hidden(&self, pid: pid_t, hidden: bool) {
        if let Some(app) = self.apps.get(&pid) {
            _ = app.handle.send(Request::SetHidden(hidden));
        }
    }

    /// Whether the application is hidden. False for unknown applications.
    pub fn is_app_hidden(&self, pid: pid_t) -> bool {
        self.apps.get(&pid).is_some_and(|app| app.info.is_hidden)
    }

    /// The main window of the active app, if any.
    fn main_window(&self) -> Option<WindowId> {
        self.main_window_tracker.main_window()
    }

    fn main_window_space(&self) -> Option<SpaceId> {
        // TODO: Optimize this with a cache or something.
        self.best_space_for_window(&self.windows.get(&self.main_window()?)?.frame_monotonic)
    }

    #[instrument(skip(self), fields())]
    pub fn update_layout(&mut self, new_wids: &[WindowId], skip_anim: bool) {
        let main_window = self.main_window();
        trace!(?main_window);
        let mut anim = Animation::new();
        let mut targets = BTreeMap::new();
        for &screen in &self.screens {
            let Some(space) = screen.space else { continue };
            if !skip_anim {
                self.layout.update_viewport_for_focus(space, screen.frame, &self.config);
            }
            let (result, groups) =
                self.layout.calculate_layout_and_groups(space, screen.frame, &self.config);

            self.group_indicators_tx
                .send(group_bars::Event::GroupsUpdated { space_id: space, groups });

            targets
                .extend(result.into_iter().map(|(wid, frame)| (wid, (frame, screen.scale_factor))));
        }
        for (wid, frame) in mem::take(&mut self.pending_frame_overrides) {
            let scale_factor = self
                .best_screen_idx_for_window(&frame)
                .and_then(|idx| self.screens.get(idx))
                .map_or(1.0, |screen| screen.scale_factor);
            targets.insert(wid, (frame, scale_factor));
        }
        for (wid, (target_frame, scale_factor)) in targets {
            if self.resizing_window == Some(wid) {
                // The user is dragging this window's edge; correct it on mouse
                // up instead.
                // TODO: A pending frame override for this window is dropped
                // here rather than deferred to mouse up.
                continue;
            }
            let Some(window) = self.windows.get_mut(&wid) else {
                // If we restored a saved state the window may not be available yet.
                continue;
            };
            let target_frame = round_to_physical(target_frame, scale_factor);
            let current_frame = window.frame_monotonic;
            if target_frame.same_as(current_frame) {
                continue;
            }
            // Some apps move a window back after we place it, which turns into
            // an event that makes us place it again. Stop writing the frame
            // once it's clear the app won't keep it.
            let now = Instant::now();
            let attempt = self.frame_attempts.entry(wid).or_insert(FrameAttempt {
                target: target_frame,
                count: 0,
                last: now,
            });
            if !attempt.target.same_as(target_frame)
                || now.duration_since(attempt.last) > FRAME_ATTEMPT_RESET
            {
                *attempt = FrameAttempt {
                    target: target_frame,
                    count: 0,
                    last: now,
                };
            }
            attempt.last = now;
            attempt.count = attempt.count.saturating_add(1);
            if attempt.count > MAX_FRAME_ATTEMPTS {
                if attempt.count == MAX_FRAME_ATTEMPTS + 1 {
                    warn!(?wid, ?current_frame, ?target_frame, "Giving up on window frame");
                }
                continue;
            }
            let Some(app) = self.apps.get(&wid.pid) else {
                continue;
            };
            let txid = window.next_txid();
            trace!(?wid, ?current_frame, ?target_frame);
            let is_new = new_wids.contains(&wid);
            anim.add_window(&app.handle, wid, current_frame, target_frame, is_new, txid);
            window.frame_monotonic = target_frame;
        }
        // If the user is doing something with the mouse we don't want to
        // animate on top of that.
        let skip_anim =
            skip_anim || !self.config.settings.animate || self.layout.has_active_scroll_animation();
        if let Some(tx) = &self.animation_tx
            && !anim.is_empty()
        {
            let message = if skip_anim {
                AnimationMessage::SkipToEnd(anim)
            } else {
                AnimationMessage::Replace(anim)
            };
            if let Err(err) = tx.send(message) {
                error!("Animation manager exited unexpectedly");
                match err.0 {
                    AnimationMessage::Replace(animation) => animation.skip_to_end(),
                    AnimationMessage::SkipToEnd(animation) => animation.skip_to_end(),
                }
            }
        } else {
            anim.skip_to_end();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use itertools::Itertools;
    use objc2_core_foundation::{CGPoint, CGSize};
    use test_log::test;

    use super::testing::*;
    use super::*;
    use crate::actor::app::Request;
    use crate::actor::layout::LayoutManager;
    use crate::model::Direction;
    use crate::sys::window_server::WindowServerId;

    #[test]
    fn it_ignores_stale_resize_events() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        let requests = apps.requests();
        assert!(!requests.is_empty());
        let events_1 = apps.simulate_events_for_requests(requests);

        reactor.handle_events(apps.make_app(2, make_windows(2)));
        assert!(!apps.requests().is_empty());

        for event in dbg!(events_1) {
            reactor.handle_event(event);
        }
        let requests = apps.requests();
        assert!(
            requests.is_empty(),
            "got requests when there should have been none: {requests:?}"
        );
    }

    #[test]
    fn it_sends_layout_animation_to_manager() {
        let mut apps = Apps::new();
        let (mut reactor, mut animation_rx) =
            Reactor::new_for_test_with_animation(LayoutManager::new_for_test(), true);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);

        assert!(
            apps.requests().is_empty(),
            "layout should be handed to the animation manager, not sent directly to app actors"
        );
        assert!(matches!(
            animation_rx.try_recv(),
            Ok(animation::Message::Replace(_))
        ));
    }

    mod scratchpad {
        use std::sync::Mutex;

        use pretty_assertions::assert_eq;
        use test_log::test;

        use super::*;
        use crate::config::{WindowRule, WindowRuleConditions};
        use crate::sys::window_server::WindowServerInfo;

        const SCREEN: CGRect = CGRect {
            origin: CGPoint { x: 0., y: 0. },
            size: CGSize { width: 1000., height: 1000. },
        };
        const FRAME: FractionalRect = FractionalRect {
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.4,
        };
        /// `FRAME` on `SCREEN`.
        const SHOWN: CGRect = CGRect {
            origin: CGPoint { x: 100., y: 200. },
            size: CGSize { width: 500., height: 400. },
        };
        fn tiled() -> WindowId {
            WindowId::new(1, 1)
        }
        fn pad() -> WindowId {
            WindowId::new(2, 1)
        }
        fn launched_pad() -> WindowId {
            WindowId::new(3, 1)
        }

        fn rule(app_id: &str, name: &str, frame: Option<FractionalRect>) -> WindowRule {
            WindowRule {
                conditions: WindowRuleConditions {
                    app_id: Some(app_id.into()),
                    ..Default::default()
                },
                float: None,
                scratchpad: Some(name.into()),
                frame,
            }
        }

        fn config(window_rules: Vec<WindowRule>) -> Arc<Config> {
            let mut config = Config::default();
            config.settings.default_disable = false;
            config.settings.animate = false;
            config.window_rules = window_rules;
            Arc::new(config)
        }

        fn toggle(name: &str, launch: Option<&str>) -> Event {
            Event::Command(Command::Reactor(ReactorCommand::ToggleScratchpad {
                name: name.into(),
                launch: launch.map(Into::into),
            }))
        }

        /// Records the actions it is asked to perform.
        #[derive(Default, Clone)]
        struct FakeSystem {
            launches: Arc<Mutex<Vec<String>>>,
            launch_ids: Arc<Mutex<Vec<PendingShowId>>>,
            moves: Arc<Mutex<Vec<(WindowId, SpaceId, SpaceMoveId)>>>,
            moves_unsupported: Arc<Mutex<bool>>,
            launches_fail: Arc<Mutex<bool>>,
        }

        impl SystemActions for FakeSystem {
            fn launch_app(
                &mut self,
                _name: &str,
                bundle_id: &str,
                id: PendingShowId,
            ) -> Result<(), crate::sys::app::LaunchError> {
                crate::sys::app::validate_bundle_id(bundle_id)?;
                if *self.launches_fail.lock().unwrap() {
                    return Err(crate::sys::app::LaunchError::Spawn("no threads".into()));
                }
                self.launches.lock().unwrap().push(bundle_id.to_owned());
                self.launch_ids.lock().unwrap().push(id);
                Ok(())
            }

            fn move_window_to_space(
                &mut self,
                wid: WindowId,
                wsid: WindowServerId,
                space: SpaceId,
                id: SpaceMoveId,
            ) -> Result<(), crate::sys::space_move::SpaceMoveError> {
                assert!(wsid.as_u32() > 0);
                if *self.moves_unsupported.lock().unwrap() {
                    return Err(crate::sys::space_move::SpaceMoveError::Unsupported);
                }
                self.moves.lock().unwrap().push((wid, space, id));
                Ok(())
            }
        }

        struct Test {
            apps: Apps,
            reactor: Reactor,
            system: FakeSystem,
            raise_rx: mpsc::UnboundedReceiver<(Span, raise::Event)>,
            windows: Vec<(pid_t, WindowInfo)>,
        }

        impl Test {
            /// App 1 has two tiled windows and is focused. App 2
            /// (`com.testapp2`) has the scratchpad window "k". App 3
            /// (`com.testapp3`) is not running and has the scratchpad "l".
            fn new() -> Test {
                Self::with_rules(vec![
                    rule("com.testapp2", "k", Some(FRAME)),
                    rule("com.testapp3", "l", Some(FRAME)),
                ])
            }

            fn with_rules(rules: Vec<WindowRule>) -> Test {
                let mut reactor = reactor_with_one_screen();
                reactor.handle_event(Event::ConfigChanged(config(rules)));
                let (raise_manager_tx, raise_rx) = mpsc::unbounded_channel();
                reactor.raise_manager_tx = raise_manager_tx;
                let system = FakeSystem::default();
                reactor.system = Box::new(system.clone());
                let mut test = Test {
                    apps: Apps::new(),
                    reactor,
                    system,
                    raise_rx,
                    windows: vec![],
                };
                test.launch(1, make_windows(2), true);
                test.launch(2, vec![make_window(5)], false);
                test.reactor.handle_event(Event::StartupComplete);
                test.settle();
                test
            }

            fn launch(&mut self, pid: pid_t, windows: Vec<WindowInfo>, focus: bool) {
                self.windows.extend(windows.iter().map(|w| (pid, w.clone())));
                let main = windows.first().map(|_| WindowId::new(pid, 1));
                let events = self.apps.make_app_with_opts(pid, windows, main, focus);
                self.reactor.handle_events(events);
                self.update_on_screen(|_| true);
                if focus {
                    self.focus_app(pid);
                }
            }

            fn focus_app(&mut self, pid: pid_t) {
                self.reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
                self.reactor.handle_event(Event::ApplicationGloballyActivated(pid));
            }

            /// Sends the complete list of visible windows.
            fn update_on_screen(&mut self, visible: impl Fn(WindowServerId) -> bool) {
                let info = self
                    .windows
                    .iter()
                    .filter_map(|(pid, w)| {
                        let id = w.sys_id?;
                        visible(id).then_some(WindowServerInfo {
                            pid: *pid,
                            id,
                            layer: 0,
                            frame: w.frame,
                        })
                    })
                    .collect();
                self.reactor.handle_event(Event::WindowsOnScreenUpdated {
                    pid: None,
                    on_screen: WindowsOnScreen::new(info),
                });
            }

            fn settle(&mut self) {
                self.apps.simulate_until_quiet(&mut self.reactor);
                while self.raise_rx.try_recv().is_ok() {}
            }

            fn focus_requests(&mut self) -> Vec<WindowId> {
                let mut focus = vec![];
                while let Ok((_, msg)) = self.raise_rx.try_recv() {
                    if let raise::Event::RaiseRequest(req) = msg {
                        focus.extend(req.focus_window.map(|(wid, _)| wid));
                    }
                }
                focus
            }

            fn launches(&self) -> Vec<String> {
                self.system.launches.lock().unwrap().clone()
            }

            fn last_launch_id(&self) -> PendingShowId {
                *self.system.launch_ids.lock().unwrap().last().expect("a launch")
            }

            fn moves(&self) -> Vec<(WindowId, SpaceId, SpaceMoveId)> {
                self.system.moves.lock().unwrap().clone()
            }

            fn set_screen_spaces(&mut self, spaces: Vec<Option<ScreenSpace>>) {
                self.reactor.handle_event(Event::ScreenSpacesChanged(spaces));
            }

            fn hide_pad(&mut self) {
                self.focus_app(2);
                self.reactor.handle_event(toggle("k", None));
                self.settle();
                assert!(self.reactor.is_app_hidden(2));
            }
        }

        fn has_frame(requests: &[(pid_t, Request)], wid: WindowId, frame: CGRect) -> bool {
            requests.iter().any(|(_, req)| {
                matches!(req, Request::SetWindowFrame(w, f, _) if *w == wid && *f == frame)
            })
        }

        fn has_set_hidden(requests: &[(pid_t, Request)], pid: pid_t, hidden: bool) -> bool {
            requests
                .iter()
                .any(|(p, req)| *p == pid && matches!(req, Request::SetHidden(h) if *h == hidden))
        }

        #[test]
        fn scratchpad_window_floats_outside_the_tree() {
            let t = Test::new();
            let space = SpaceId::new(1);
            assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(pad()));
            assert_eq!(
                t.reactor.layout.floating_windows_in_space(space),
                [pad()].into_iter().collect()
            );
            let frames = window_frames(&t.reactor);
            let half = |x| CGRect::new(CGPoint::new(x, 0.), CGSize::new(500., 1000.));
            assert_eq!(frames[0], (tiled(), half(0.)));
            assert_eq!(frames[1], (WindowId::new(1, 2), half(500.)));
            assert_eq!(frames[2], (pad(), make_window(5).frame), "not laid out");
        }

        #[test]
        fn toggle_hides_the_visible_focused_window() {
            let mut t = Test::new();
            t.focus_app(2);
            t.settle();
            t.reactor.handle_event(toggle("k", Some("com.testapp2")));
            let requests = t.apps.tagged_requests();
            assert!(
                matches!(requests[..], [(2, Request::SetHidden(true))]),
                "{requests:?}"
            );
            assert!(t.launches().is_empty());
            assert!(t.focus_requests().is_empty());
        }

        #[test]
        fn toggle_shows_the_hidden_app_with_the_rule_frame() {
            let mut t = Test::new();
            t.hide_pad();
            t.focus_app(1);

            t.reactor.handle_event(toggle("k", Some("com.testapp2")));
            // Nothing answers the unhide here; showing must not wait for it.
            let requests = t.apps.tagged_requests();
            assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
            assert!(has_frame(&requests, pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [pad()]);
            assert!(t.launches().is_empty());
            assert!(
                !requests.iter().any(|(pid, _)| *pid == 1),
                "tiled windows are left alone: {requests:?}"
            );
        }

        #[test]
        fn toggle_shows_a_manually_hidden_app() {
            let mut t = Test::new();
            t.focus_app(2);
            t.reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
            t.settle();
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
            assert!(has_frame(&requests, pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [pad()]);
        }

        #[test]
        fn toggle_shows_an_unfocused_window_without_unhiding() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            assert!(
                !requests.iter().any(|(_, r)| matches!(r, Request::SetHidden(_))),
                "{requests:?}"
            );
            assert!(has_frame(&requests, pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [pad()]);
        }

        #[test]
        fn toggle_unminimizes_a_window_that_is_not_on_screen() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("k", None));
            t.settle();
            assert!(t.apps.unminimized.is_empty(), "the window is visible");

            // A minimized window is not on screen.
            t.update_on_screen(|wsid| wsid != WindowServerId::new(5));
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            assert!(
                requests.iter().any(|(pid, r)| *pid == 2 && matches!(r, Request::Unminimize(w) if *w == pad())),
                "{requests:?}"
            );
            assert_eq!(t.focus_requests(), [pad()]);
        }

        #[test]
        fn toggle_shows_a_focused_window_that_is_not_on_screen() {
            // A window on another space is missing from the visible windows.
            let mut t = Test::new();
            t.focus_app(2);
            t.update_on_screen(|wsid| wsid != WindowServerId::new(5));
            t.settle();
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            assert!(has_frame(&requests, pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [pad()]);
            assert!(!has_set_hidden(&requests, 2, true), "{requests:?}");
        }

        #[test]
        fn toggle_twice_hides_the_window_it_showed() {
            let mut t = Test::new();
            t.hide_pad();
            t.focus_app(1);
            t.reactor.handle_event(toggle("k", None));
            t.settle();
            assert!(!t.reactor.is_app_hidden(2));
            // The raise made the scratchpad app active.
            t.focus_app(2);
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            assert!(
                matches!(requests[..], [(2, Request::SetHidden(true))]),
                "{requests:?}"
            );
        }

        #[test]
        fn toggle_launches_and_shows_the_window_when_it_appears() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            assert_eq!(t.launches(), ["com.testapp3"]);
            assert!(t.apps.tagged_requests().is_empty());
            assert!(t.focus_requests().is_empty());

            t.launch(3, vec![make_window(7)], false);
            let requests = t.apps.tagged_requests();
            assert!(has_frame(&requests, launched_pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [launched_pad()]);
            assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
            let events = t.apps.simulate_events_for_tagged_requests(requests);
            t.reactor.handle_events(events);

            // The window is shown once.
            t.reactor.handle_event(Event::WindowsDiscovered {
                pid: 3,
                new: vec![],
                known_visible: vec![launched_pad()],
            });
            assert!(t.focus_requests().is_empty());
            assert_eq!(t.launches(), ["com.testapp3"]);
        }

        #[test]
        fn window_created_after_launch_is_shown() {
            let mut t = Test::new();
            t.launch(3, vec![], false);
            t.settle();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            assert_eq!(t.launches(), ["com.testapp3"]);

            let window = make_window(7);
            t.windows.push((3, window.clone()));
            t.reactor
                .handle_event(Event::WindowCreated(launched_pad(), window, MouseState::Up));
            t.update_on_screen(|_| true);
            t.reactor.handle_event(Event::WindowBecameVisible(launched_pad()));
            let requests = t.apps.tagged_requests();
            assert!(has_frame(&requests, launched_pad(), SHOWN), "{requests:?}");
            assert_eq!(t.focus_requests(), [launched_pad()]);
        }

        #[test]
        fn toggle_without_window_or_launch_only_warns() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", None));
            t.reactor.handle_event(toggle("unknown", None));
            assert!(t.launches().is_empty());
            assert!(t.apps.tagged_requests().is_empty());
            assert!(t.focus_requests().is_empty());

            // The window is not shown when the app is opened by other means.
            t.launch(3, vec![make_window(7)], false);
            assert!(t.focus_requests().is_empty());
            assert!(!has_frame(&t.apps.tagged_requests(), launched_pad(), SHOWN));
        }

        #[test]
        fn failed_launch_does_not_show_the_window_later() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("-a")));
            assert!(t.launches().is_empty());
            t.launch(3, vec![make_window(7)], false);
            assert!(t.focus_requests().is_empty());
        }

        #[test]
        fn window_appearing_after_the_show_expired_is_not_shown() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            let id = t.last_launch_id();
            assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), Some(id));
            t.reactor.handle_event(Event::ScratchpadShowExpired(id));

            t.launch(3, vec![make_window(7)], false);
            assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
            assert!(t.focus_requests().is_empty());
            assert!(!has_frame(&t.apps.tagged_requests(), launched_pad(), SHOWN));
        }

        #[test]
        fn window_appearing_in_time_is_shown_once() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            let id = t.last_launch_id();
            t.launch(3, vec![make_window(7)], false);
            assert_eq!(t.focus_requests(), [launched_pad()]);
            t.settle();

            // The expiry of a show that already happened changes nothing.
            t.reactor.handle_event(Event::ScratchpadShowExpired(id));
            t.reactor.handle_event(Event::WindowsDiscovered {
                pid: 3,
                new: vec![],
                known_visible: vec![launched_pad()],
            });
            assert!(t.focus_requests().is_empty());
            assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
        }

        #[test]
        fn expiry_of_an_earlier_launch_keeps_a_later_one() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            let first = t.last_launch_id();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            assert_ne!(t.last_launch_id(), first);
            t.reactor.handle_event(Event::ScratchpadShowExpired(first));

            t.launch(3, vec![make_window(7)], false);
            assert_eq!(t.focus_requests(), [launched_pad()]);
        }

        #[test]
        fn quitting_the_launched_app_drops_the_pending_show() {
            let mut t = Test::new();
            t.launch(3, vec![], false);
            t.settle();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            t.reactor.handle_event(toggle("k2", Some("com.testapp2")));
            assert!(t.reactor.layout.pending_scratchpad_show("l").is_some());
            t.reactor.handle_event(Event::ApplicationTerminated(3));
            t.reactor.handle_event(Event::ApplicationThreadTerminated(3));
            t.settle();
            assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), None);
            assert!(
                t.reactor.layout.pending_scratchpad_show("k2").is_some(),
                "shows that launched other apps are kept"
            );
        }

        #[test]
        fn failed_launch_is_recorded_and_replayed() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("m", Some("com.testapp4")));
            *t.system.launches_fail.lock().unwrap() = true;
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), None);
            let trace = replay::tests::recorded_trace(&mut t.reactor);
            assert!(trace.contains("ScratchpadShowExpired"), "{trace}");
            let mut replayed = replay::tests::replay_trace(&trace);
            assert_eq!(replayed.layout.pending_scratchpad_show("l"), None);
            assert_eq!(
                replayed.layout.pending_scratchpad_show("m"),
                t.reactor.layout.pending_scratchpad_show("m")
            );
            assert!(t.reactor.layout.pending_scratchpad_show("m").is_some());

            // The window that appears later is shown by neither.
            for reactor in [&mut t.reactor, &mut replayed] {
                let mut apps = Apps::new();
                let events = apps.make_app_with_opts(3, vec![make_window(7)], None, false);
                reactor.handle_events(events);
                assert!(!apps.requests().iter().any(|r| matches!(r, Request::SetWindowFrame(..))));
            }
        }

        #[test]
        fn show_expiry_is_recorded_and_replayed() {
            let mut t = Test::new();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            t.reactor.handle_event(toggle("m", Some("com.testapp4")));
            let id = t.last_launch_id();
            t.reactor.handle_event(Event::ScratchpadShowExpired(id));
            let trace = replay::tests::recorded_trace(&mut t.reactor);
            assert!(trace.contains("ScratchpadShowExpired"), "{trace}");
            let replayed = replay::tests::replay_trace(&trace);
            assert!(replayed.layout.pending_scratchpad_show("l").is_some());
            assert_eq!(replayed.layout.pending_scratchpad_show("m"), None);
        }

        #[test]
        fn destroyed_window_and_terminated_app_are_forgotten() {
            let mut t = Test::new();
            t.reactor.handle_event(Event::WindowDestroyed(pad()));
            assert_eq!(t.reactor.layout.scratchpad_window("k"), None);
            t.reactor.handle_event(toggle("k", Some("com.testapp2")));
            assert_eq!(t.launches(), ["com.testapp2"]);

            t.launch(3, vec![make_window(7)], false);
            t.settle();
            assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
            t.reactor.handle_event(Event::ApplicationTerminated(3));
            t.reactor.handle_event(Event::ApplicationThreadTerminated(3));
            t.settle();
            assert_eq!(t.reactor.layout.scratchpad_window("l"), None);
        }

        #[test]
        fn toggle_focus_floating_skips_scratchpad_windows() {
            let mut t = Test::new();
            // Focusing the scratchpad must not make it the floating window to
            // return to.
            t.focus_app(2);
            t.focus_app(1);
            t.settle();
            t.reactor.handle_event(Event::Command(Command::Layout(
                LayoutCommand::ToggleFocusFloating,
            )));
            assert!(t.focus_requests().is_empty());
            assert!(t.apps.tagged_requests().is_empty());
        }

        #[test]
        fn toggle_window_floating_keeps_scratchpad_floating() {
            let mut t = Test::new();
            t.focus_app(2);
            t.settle();
            t.reactor.handle_event(Event::Command(Command::Layout(
                LayoutCommand::ToggleWindowFloating,
            )));
            t.settle();
            assert_eq!(
                t.reactor.layout.floating_windows_in_space(SpaceId::new(1)),
                [pad()].into_iter().collect()
            );
            assert_eq!(window_frames(&t.reactor)[2], (pad(), make_window(5).frame));
        }

        #[test]
        fn invalid_rules_from_ipc_are_sanitized() {
            let bad_frame = FractionalRect {
                x: f64::NAN,
                y: 0.5,
                width: f64::INFINITY,
                height: 0.5,
            };
            let blank_name = rule("com.testapp1", " ", None);
            let mut t =
                Test::with_rules(vec![blank_name, rule("com.testapp2", "k", Some(bad_frame))]);
            t.reactor.handle_event(toggle("k", None));
            let requests = t.apps.tagged_requests();
            let bottom_half = CGRect::new(CGPoint::new(0., 500.), CGSize::new(1000., 500.));
            assert!(has_frame(&requests, pad(), bottom_half), "{requests:?}");
            // The rule with a blank name is dropped, so app 1 still tiles.
            assert_eq!(
                t.reactor.layout.floating_windows_in_space(SpaceId::new(1)),
                [pad()].into_iter().collect()
            );
        }

        #[test]
        fn default_frame_is_used_without_frame() {
            let mut t = Test::with_rules(vec![rule("com.testapp2", "k", None)]);
            t.reactor.handle_event(toggle("k", None));
            let default = FractionalRect::DEFAULT.to_frame(SCREEN);
            let requests = t.apps.tagged_requests();
            assert!(has_frame(&requests, pad(), default), "{requests:?}");
        }

        #[test]
        fn toggle_is_recorded_and_replayed_without_launching() {
            let mut t = Test::new();
            t.hide_pad();
            t.reactor.handle_event(toggle("l", Some("com.testapp3")));
            let trace = replay::tests::recorded_trace(&mut t.reactor);
            assert!(trace.contains("toggle_scratchpad"), "{trace}");
            let replayed = replay::tests::replay_trace(&trace);
            assert_eq!(replayed.layout.scratchpad_window("k"), Some(pad()));
            assert!(replayed.is_app_hidden(2));
        }

        mod space_moves {
            use pretty_assertions::assert_eq;
            use test_log::test;

            use super::edge_cases::{frames_of, window_at};
            use super::*;

            fn current(id: u64) -> Option<ScreenSpace> {
                Some(ScreenSpace {
                    id: SpaceId::new(id),
                    fullscreen: false,
                })
            }

            fn fullscreen(id: u64) -> Option<ScreenSpace> {
                Some(ScreenSpace {
                    id: SpaceId::new(id),
                    fullscreen: true,
                })
            }

            fn not_on_screen(wsid: WindowServerId) -> bool {
                wsid != WindowServerId::new(5)
            }

            /// The scratchpad window is on another space of the screen.
            fn pad_on_another_space() -> Test {
                let mut t = Test::new();
                t.set_screen_spaces(vec![current(1)]);
                t.update_on_screen(not_on_screen);
                t.settle();
                t
            }

            #[test]
            fn window_on_another_space_is_moved_and_then_shown() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                assert_eq!(t.moves().len(), 1);
                assert_eq!(t.moves()[0].0, pad());
                assert_eq!(t.moves()[0].1, SpaceId::new(1));
                let requests = t.apps.tagged_requests();
                assert!(frames_of(&requests, pad()).is_empty(), "{requests:?}");
                assert!(t.focus_requests().is_empty());
                assert!(t.launches().is_empty());

                // Other windows changing does not end the wait.
                t.update_on_screen(|wsid| not_on_screen(wsid) && wsid != WindowServerId::new(1));
                assert!(t.focus_requests().is_empty());

                t.update_on_screen(|_| true);
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);

                // The window is shown once.
                t.update_on_screen(|_| true);
                assert!(t.focus_requests().is_empty());
                assert!(t.reactor.pending_space_moves.is_empty());
            }

            #[test]
            fn partial_window_update_ends_the_wait() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", None));
                t.reactor.handle_event(Event::WindowsOnScreenUpdated {
                    pid: Some(2),
                    on_screen: WindowsOnScreen::new(vec![WindowServerInfo {
                        pid: 2,
                        id: WindowServerId::new(5),
                        layer: 0,
                        frame: make_window(5).frame,
                    }]),
                });
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn move_timeout_shows_the_window_without_the_move() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", None));
                let first = t.moves()[0].2;
                // A second toggle before the window arrives starts a new move.
                t.reactor.handle_event(toggle("k", None));
                let second = t.moves()[1].2;
                assert_ne!(first, second);

                t.reactor.handle_event(Event::ScratchpadMoveEnded(first));
                assert!(
                    t.focus_requests().is_empty(),
                    "an older move ending changes nothing"
                );
                assert!(frames_of(&t.apps.tagged_requests(), pad()).is_empty());

                t.reactor.handle_event(Event::ScratchpadMoveEnded(second));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);

                t.reactor.handle_event(Event::ScratchpadMoveEnded(second));
                assert!(t.focus_requests().is_empty());
            }

            #[test]
            fn unsupported_move_shows_the_window_at_once() {
                let mut t = pad_on_another_space();
                *t.system.moves_unsupported.lock().unwrap() = true;
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
                assert!(t.reactor.pending_space_moves.is_empty());

                let trace = replay::tests::recorded_trace(&mut t.reactor);
                assert!(trace.contains("ScratchpadMoveEnded"), "{trace}");
                let replayed = replay::tests::replay_trace(&trace);
                assert!(replayed.pending_space_moves.is_empty());
            }

            #[test]
            fn window_is_not_moved_to_a_fullscreen_space() {
                let mut t = pad_on_another_space();
                t.set_screen_spaces(vec![fullscreen(1)]);
                t.reactor.handle_event(toggle("k", None));
                assert!(t.moves().is_empty());
                assert_eq!(frames_of(&t.apps.tagged_requests(), pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn window_is_not_moved_without_a_current_space() {
                let mut t = pad_on_another_space();
                t.set_screen_spaces(vec![None]);
                t.reactor.handle_event(toggle("k", None));
                assert!(t.moves().is_empty());
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn visible_window_is_not_moved() {
                let mut t = Test::new();
                t.set_screen_spaces(vec![current(1)]);
                t.reactor.handle_event(toggle("k", None));
                assert!(t.moves().is_empty());
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn hidden_app_window_is_moved_unhidden_and_shown() {
                let mut t = Test::new();
                t.set_screen_spaces(vec![current(1)]);
                t.hide_pad();
                t.focus_app(1);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                assert_eq!(t.moves().len(), 1);
                let requests = t.apps.tagged_requests();
                assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
                assert!(frames_of(&requests, pad()).is_empty(), "{requests:?}");
                assert!(t.focus_requests().is_empty());

                // The window server reports the window once the app is shown.
                t.update_on_screen(|_| true);
                assert_eq!(frames_of(&t.apps.tagged_requests(), pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn window_is_moved_to_the_space_of_the_active_screen() {
                let mut t = Test::two_screens_with(make_window(5));
                t.set_screen_spaces(vec![current(1), current(2)]);
                t.launch(4, vec![window_at(9, 1300.)], true);
                t.update_on_screen(not_on_screen);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                assert_eq!(t.moves()[0].1, SpaceId::new(2));
            }

            #[test]
            fn window_is_moved_to_a_current_space_that_is_not_managed() {
                let mut t = pad_on_another_space();
                t.refresh(vec![None]);
                t.update_on_screen(not_on_screen);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                assert_eq!(t.moves().len(), 1);
                assert_eq!(t.moves()[0].1, SpaceId::new(1));
            }

            #[test]
            fn launched_window_on_another_space_is_moved() {
                let mut t = Test::new();
                t.set_screen_spaces(vec![current(1)]);
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                // The app opens its window on another space.
                let window = make_window(7);
                t.windows.push((3, window.clone()));
                t.update_on_screen(|wsid| wsid != WindowServerId::new(7));
                let events =
                    t.apps.make_app_without_ws_info(3, vec![window], Some(launched_pad()), false);
                t.reactor.handle_events(events);
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert_eq!(t.moves().len(), 1);
                assert_eq!(t.moves()[0].0, launched_pad());
                assert!(t.focus_requests().is_empty());

                t.update_on_screen(|_| true);
                assert_eq!(frames_of(&t.apps.tagged_requests(), launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn destroyed_window_is_not_shown_after_its_move() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", None));
                let id = t.moves()[0].2;
                t.reactor.handle_event(Event::WindowDestroyed(pad()));
                assert!(t.reactor.pending_space_moves.is_empty());
                t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                assert!(t.focus_requests().is_empty());
            }

            #[test]
            fn quitting_the_app_drops_its_move() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", None));
                t.reactor.handle_event(Event::ApplicationTerminated(2));
                t.reactor.handle_event(Event::ApplicationThreadTerminated(2));
                assert!(t.reactor.pending_space_moves.is_empty());
            }

            #[test]
            fn replay_makes_the_same_decisions_without_moving() {
                let mut t = pad_on_another_space();
                t.reactor.handle_event(toggle("k", None));
                let id = t.moves()[0].2;
                let trace = replay::tests::recorded_trace(&mut t.reactor);
                assert!(trace.contains("ScreenSpacesChanged"), "{trace}");
                let replayed = replay::tests::replay_trace(&trace);
                assert_eq!(replayed.pending_space_moves.keys().collect::<Vec<_>>(), [&pad()]);
                assert_eq!(replayed.pending_space_moves[&pad()].id, id);

                t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                let trace = replay::tests::recorded_trace(&mut t.reactor);
                let replayed = replay::tests::replay_trace(&trace);
                assert!(replayed.pending_space_moves.is_empty());
                assert_eq!(t.moves().len(), 1, "only the live reactor moved the window");
            }

            /// Events that arrive while a scratchpad window is being moved.
            mod races {
                use pretty_assertions::assert_eq;
                use test_log::test;

                use super::*;

                fn move_ids(t: &Test) -> Vec<SpaceMoveId> {
                    t.moves().into_iter().map(|(_, _, id)| id).collect()
                }

                fn pending(t: &Test) -> Vec<(WindowId, SpaceMoveId)> {
                    t.reactor.pending_space_moves.iter().map(|(wid, m)| (*wid, m.id)).collect()
                }

                fn on_screen(
                    t: &Test,
                    visible: impl Fn(WindowServerId) -> bool,
                ) -> WindowsOnScreen {
                    WindowsOnScreen::new(
                        t.windows
                            .iter()
                            .filter_map(|(pid, w)| {
                                let id = w.sys_id?;
                                visible(id).then_some(WindowServerInfo {
                                    pid: *pid,
                                    id,
                                    layer: 0,
                                    frame: w.frame,
                                })
                            })
                            .collect(),
                    )
                }

                fn screen_changed(t: &Test, visible: impl Fn(WindowServerId) -> bool) -> Event {
                    Event::ScreenParametersChanged {
                        frames: vec![SCREEN],
                        spaces: vec![Some(SpaceId::new(1))],
                        scale_factors: vec![2.0],
                        converter: CoordinateConverter::default(),
                        on_screen: on_screen(t, visible),
                    }
                }

                /// App 3 runs with the scratchpad "l" window on another space,
                /// like the "k" window of app 2.
                fn two_pads_on_other_spaces() -> Test {
                    let mut t = Test::new();
                    t.set_screen_spaces(vec![current(1)]);
                    t.launch(3, vec![make_window(7)], false);
                    t.update_on_screen(|wsid| {
                        wsid != WindowServerId::new(5) && wsid != WindowServerId::new(7)
                    });
                    t.settle();
                    assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                    t
                }

                #[test]
                fn toggle_during_a_move_moves_again_without_hiding_or_launching() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                    t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                    let requests = t.apps.tagged_requests();
                    assert!(!has_set_hidden(&requests, 2, true), "{requests:?}");
                    assert!(t.launches().is_empty());
                    let ids = move_ids(&t);
                    assert_eq!(ids.len(), 2);
                    assert_eq!(pending(&t), [(pad(), ids[1])]);

                    t.update_on_screen(|_| true);
                    assert_eq!(frames_of(&t.apps.tagged_requests(), pad()), [SHOWN]);
                    assert_eq!(t.focus_requests(), [pad()]);
                    // Neither move ending shows the window again.
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(ids[0]));
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(ids[1]));
                    assert!(t.focus_requests().is_empty());
                }

                #[test]
                fn toggle_after_the_window_arrived_hides_it() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    t.update_on_screen(|_| true);
                    assert_eq!(t.focus_requests(), [pad()]);
                    t.focus_app(2);
                    t.settle();

                    t.reactor.handle_event(toggle("k", None));
                    let requests = t.apps.tagged_requests();
                    assert!(has_set_hidden(&requests, 2, true), "{requests:?}");
                    assert_eq!(move_ids(&t).len(), 1);
                }

                #[test]
                fn window_closed_during_a_move_is_not_shown_by_later_updates() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    t.reactor.handle_event(Event::WindowDestroyed(pad()));
                    // The window server still lists the closed window.
                    t.update_on_screen(|_| true);
                    t.reactor.handle_event(screen_changed(&t, |_| true));
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    assert!(t.focus_requests().is_empty());
                    assert!(frames_of(&t.apps.tagged_requests(), pad()).is_empty());
                }

                #[test]
                fn app_hidden_during_a_move_cancels_the_show() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
                    assert!(pending(&t).is_empty());
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    assert!(frames_of(&t.apps.tagged_requests(), pad()).is_empty());
                    assert!(t.focus_requests().is_empty());

                    // The user shows the app on the current space.
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(2, false));
                    t.update_on_screen(|_| true);
                    assert!(frames_of(&t.apps.tagged_requests(), pad()).is_empty());
                    assert!(t.focus_requests().is_empty());

                    let trace = replay::tests::recorded_trace(&mut t.reactor);
                    let replayed = replay::tests::replay_trace(&trace);
                    assert!(replayed.pending_space_moves.is_empty());
                }

                #[test]
                fn hiding_another_app_during_a_move_keeps_the_wait() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
                    assert_eq!(pending(&t), [(pad(), id)]);
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    assert_eq!(t.focus_requests(), [pad()]);
                }

                #[test]
                fn unhiding_a_hidden_app_for_its_move_keeps_the_wait() {
                    let mut t = Test::new();
                    t.set_screen_spaces(vec![current(1)]);
                    t.hide_pad();
                    t.focus_app(1);
                    t.settle();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    // A report of the earlier hide arrives before the unhide.
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
                    assert_eq!(pending(&t), [(pad(), id)]);
                    // The app reports the unhide, and the shown state again.
                    t.settle();
                    assert!(!t.reactor.is_app_hidden(2));
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(2, false));
                    assert_eq!(pending(&t), [(pad(), id)]);

                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    assert_eq!(frames_of(&t.apps.tagged_requests(), pad()), [SHOWN]);
                    assert_eq!(t.focus_requests(), [pad()]);
                }

                #[test]
                fn hiding_the_app_after_it_was_unhidden_for_its_move_cancels_the_show() {
                    let mut t = Test::new();
                    t.set_screen_spaces(vec![current(1)]);
                    t.hide_pad();
                    t.focus_app(1);
                    t.settle();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    t.settle();
                    t.reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
                    assert!(pending(&t).is_empty());
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    t.update_on_screen(|_| true);
                    assert!(frames_of(&t.apps.tagged_requests(), pad()).is_empty());
                    assert!(t.focus_requests().is_empty());
                }

                #[test]
                fn space_change_that_shows_the_window_ends_the_wait() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    t.set_screen_spaces(vec![current(2)]);
                    t.reactor.handle_event(Event::SpaceChanged(
                        vec![Some(SpaceId::new(2))],
                        on_screen(&t, |_| true),
                    ));
                    assert_eq!(t.focus_requests(), [pad()]);
                    assert!(pending(&t).is_empty());
                    assert_eq!(move_ids(&t).len(), 1);
                }

                #[test]
                fn space_change_without_the_window_keeps_waiting() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    t.set_screen_spaces(vec![current(3)]);
                    t.reactor.handle_event(Event::SpaceChanged(
                        vec![Some(SpaceId::new(3))],
                        on_screen(&t, not_on_screen),
                    ));
                    assert!(t.focus_requests().is_empty());
                    assert_eq!(pending(&t), [(pad(), id)]);

                    // The next toggle moves the window to the new current space.
                    t.reactor.handle_event(toggle("k", None));
                    assert_eq!(t.moves()[1].1, SpaceId::new(3));
                }

                #[test]
                fn screen_change_that_shows_the_window_ends_the_wait() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    t.reactor.handle_event(screen_changed(&t, not_on_screen));
                    assert!(t.focus_requests().is_empty());
                    assert_eq!(pending(&t).len(), 1);

                    t.reactor.handle_event(screen_changed(&t, |_| true));
                    assert_eq!(t.focus_requests(), [pad()]);
                    assert!(pending(&t).is_empty());
                }

                #[test]
                fn toggle_that_shows_at_once_drops_the_earlier_wait() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    let id = move_ids(&t)[0];
                    // The user is now on a fullscreen space, so the window is
                    // shown where it is.
                    t.set_screen_spaces(vec![fullscreen(2)]);
                    t.reactor.handle_event(toggle("k", None));
                    assert_eq!(t.focus_requests(), [pad()]);
                    assert!(pending(&t).is_empty());

                    t.update_on_screen(|_| true);
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(id));
                    assert!(t.focus_requests().is_empty(), "shown only once");
                }

                #[test]
                fn two_scratchpads_wait_and_end_independently() {
                    let mut t = two_pads_on_other_spaces();
                    t.reactor.handle_event(toggle("k", None));
                    t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                    assert!(t.launches().is_empty());
                    let moves = t.moves();
                    assert_eq!(
                        moves.iter().map(|(wid, space, _)| (*wid, *space)).collect::<Vec<_>>(),
                        [(pad(), SpaceId::new(1)), (launched_pad(), SpaceId::new(1))]
                    );
                    let (k_id, l_id) = (moves[0].2, moves[1].2);
                    assert_ne!(k_id, l_id);

                    t.reactor.handle_event(Event::ScratchpadMoveEnded(k_id));
                    assert_eq!(t.focus_requests(), [pad()]);
                    assert_eq!(pending(&t), [(launched_pad(), l_id)]);

                    t.update_on_screen(|wsid| wsid == WindowServerId::new(7));
                    let requests = t.apps.tagged_requests();
                    assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                    assert_eq!(t.focus_requests(), [launched_pad()]);
                    assert!(pending(&t).is_empty());
                }

                #[test]
                fn two_scratchpads_arriving_together_are_both_shown() {
                    let mut t = two_pads_on_other_spaces();
                    t.reactor.handle_event(toggle("k", None));
                    t.reactor.handle_event(toggle("l", None));
                    t.update_on_screen(|_| true);
                    let requests = t.apps.tagged_requests();
                    assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                    assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                    let mut focused = t.focus_requests();
                    focused.sort();
                    assert_eq!(focused, [pad(), launched_pad()]);
                    assert!(pending(&t).is_empty());
                }

                #[test]
                fn quitting_one_app_keeps_the_move_of_another() {
                    let mut t = two_pads_on_other_spaces();
                    t.reactor.handle_event(toggle("k", None));
                    t.reactor.handle_event(toggle("l", None));
                    let k_id = move_ids(&t)[0];
                    t.reactor.handle_event(Event::ApplicationTerminated(3));
                    t.reactor.handle_event(Event::ApplicationThreadTerminated(3));
                    assert_eq!(pending(&t), [(pad(), k_id)]);
                }

                #[test]
                fn move_ids_stay_unique_after_a_failed_move() {
                    let mut t = pad_on_another_space();
                    *t.system.moves_unsupported.lock().unwrap() = true;
                    t.reactor.handle_event(toggle("k", None));
                    assert_eq!(t.focus_requests(), [pad()]);
                    *t.system.moves_unsupported.lock().unwrap() = false;
                    t.reactor.handle_event(toggle("k", None));
                    // The failed move took id 0; a stale end of it must not
                    // show the window early.
                    assert_eq!(move_ids(&t), [SpaceMoveId(1)]);
                    t.reactor.handle_event(Event::ScratchpadMoveEnded(SpaceMoveId(0)));
                    assert!(t.focus_requests().is_empty());
                    assert_eq!(pending(&t).len(), 1);
                }

                #[test]
                fn active_screen_without_a_recorded_space_shows_at_once() {
                    let mut t = Test::two_screens_with(make_window(5));
                    // Only the first screen's space is known.
                    t.set_screen_spaces(vec![current(1)]);
                    t.launch(4, vec![window_at(9, 1300.)], true);
                    t.update_on_screen(not_on_screen);
                    t.settle();
                    t.reactor.handle_event(toggle("k", None));
                    assert!(t.moves().is_empty());
                    assert_eq!(t.focus_requests(), [pad()]);
                }

                #[test]
                fn fullscreen_space_of_another_screen_does_not_stop_the_move() {
                    let mut t = Test::two_screens_with(make_window(5));
                    t.set_screen_spaces(vec![fullscreen(1), current(2)]);
                    t.launch(4, vec![window_at(9, 1300.)], true);
                    t.update_on_screen(not_on_screen);
                    t.settle();
                    t.reactor.handle_event(toggle("k", None));
                    assert_eq!(
                        t.moves().iter().map(|(_, space, _)| *space).collect::<Vec<_>>(),
                        [SpaceId::new(2)]
                    );
                }

                #[test]
                fn replay_of_a_move_and_its_arrival_makes_the_same_decisions() {
                    let mut t = pad_on_another_space();
                    t.reactor.handle_event(toggle("k", None));
                    t.update_on_screen(|_| true);
                    assert_eq!(t.focus_requests(), [pad()]);
                    let trace = replay::tests::recorded_trace(&mut t.reactor);
                    let replayed = replay::tests::replay_trace(&trace);
                    assert!(replayed.pending_space_moves.is_empty());
                    assert_eq!(replayed.next_space_move_id, t.reactor.next_space_move_id);
                    assert_eq!(
                        replayed.windows[&pad()].frame_monotonic,
                        t.reactor.windows[&pad()].frame_monotonic
                    );
                    assert_eq!(t.moves().len(), 1, "the replay did not move the window");
                }

                #[test]
                fn replay_does_not_wait_where_the_live_reactor_did_not_move() {
                    let mut t = pad_on_another_space();
                    t.set_screen_spaces(vec![fullscreen(1)]);
                    t.reactor.handle_event(toggle("k", None));
                    assert!(t.moves().is_empty());
                    let trace = replay::tests::recorded_trace(&mut t.reactor);
                    let replayed = replay::tests::replay_trace(&trace);
                    assert!(replayed.pending_space_moves.is_empty());
                    assert_eq!(replayed.next_space_move_id, 0);
                }
            }
        }

        mod edge_cases {
            use pretty_assertions::assert_eq;
            use test_log::test;

            use super::*;

            const SCREEN2: CGRect = CGRect {
                origin: CGPoint { x: 1000., y: 0. },
                size: CGSize { width: 1000., height: 1000. },
            };

            pub(super) fn window_at(idx: usize, x: f64) -> WindowInfo {
                let mut window = make_window(idx);
                window.frame.origin.x = x;
                window
            }

            impl Test {
                /// Like [`Test::with_rules`], with the given screens and the
                /// windows of the scratchpad app 2.
                fn with_screens(
                    rules: Vec<WindowRule>,
                    frames: Vec<CGRect>,
                    spaces: Vec<Option<SpaceId>>,
                    pad_windows: Vec<WindowInfo>,
                ) -> Test {
                    let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
                    let scale_factors = vec![2.0; frames.len()];
                    reactor.handle_event(Event::ScreenParametersChanged {
                        frames,
                        spaces,
                        scale_factors,
                        converter: CoordinateConverter::default(),
                        on_screen: Default::default(),
                    });
                    reactor.handle_event(Event::ConfigChanged(config(rules)));
                    let (raise_manager_tx, raise_rx) = mpsc::unbounded_channel();
                    reactor.raise_manager_tx = raise_manager_tx;
                    let system = FakeSystem::default();
                    reactor.system = Box::new(system.clone());
                    let mut test = Test {
                        apps: Apps::new(),
                        reactor,
                        system,
                        raise_rx,
                        windows: vec![],
                    };
                    test.launch(1, make_windows(2), true);
                    test.launch(2, pad_windows, false);
                    test.reactor.handle_event(Event::StartupComplete);
                    test.settle();
                    test
                }

                pub(super) fn two_screens_with(pad_window: WindowInfo) -> Test {
                    Self::two_screens(pad_window)
                }

                fn two_screens(pad_window: WindowInfo) -> Test {
                    Self::with_screens(
                        vec![
                            rule("com.testapp2", "k", Some(FRAME)),
                            rule("com.testapp3", "l", Some(FRAME)),
                        ],
                        vec![SCREEN, SCREEN2],
                        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
                        vec![pad_window],
                    )
                }

                /// What the window server does after a config reload or a space
                /// change: reports the spaces and asks every app for its windows.
                pub(super) fn refresh(&mut self, spaces: Vec<Option<SpaceId>>) {
                    let info = self
                        .windows
                        .iter()
                        .filter_map(|(pid, w)| {
                            Some(WindowServerInfo {
                                pid: *pid,
                                id: w.sys_id?,
                                layer: 0,
                                frame: w.frame,
                            })
                        })
                        .collect();
                    self.reactor
                        .handle_event(Event::SpaceChanged(spaces, WindowsOnScreen::new(info)));
                    self.apps.simulate_until_quiet(&mut self.reactor);
                }

                fn set_main_window(&mut self, wid: WindowId) {
                    self.reactor.handle_event(Event::ApplicationMainWindowChanged(
                        wid.pid,
                        Some(wid),
                        Quiet::No,
                    ));
                }

                fn add_window(&mut self, wid: WindowId, window: WindowInfo) {
                    self.windows.push((wid.pid, window.clone()));
                    self.reactor.handle_event(Event::WindowCreated(wid, window, MouseState::Up));
                    self.update_on_screen(|_| true);
                    self.reactor.handle_event(Event::WindowBecameVisible(wid));
                }
            }

            impl Test {
                /// The user moves the window.
                fn drag(&mut self, wid: WindowId, frame: CGRect) {
                    let txid = self.reactor.windows[&wid].last_sent_txid;
                    self.reactor.handle_event(Event::WindowFrameChanged(
                        wid,
                        frame,
                        txid,
                        Requested(false),
                        Some(MouseState::Up),
                    ));
                    self.settle();
                }
            }

            #[test]
            fn toggle_uses_the_screen_of_the_focused_window() {
                let mut t = Test::two_screens(make_window(5));
                t.launch(4, vec![window_at(9, 1300.)], true);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [FRAME.to_frame(SCREEN2)]);
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn focused_window_parked_off_screen_is_shown() {
                let mut t = Test::new();
                let parked = CGRect::new(CGPoint::new(-5000., 100.), CGSize::new(50., 50.));
                t.drag(pad(), parked);
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert!(!has_set_hidden(&requests, 2, true), "{requests:?}");
            }

            #[test]
            fn window_moved_to_another_screen_is_registered_under_the_renamed_rule() {
                let mut t = Test::two_screens(make_window(5));
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp2",
                    "k2",
                    Some(FRAME),
                )])));
                assert_eq!(t.reactor.layout.scratchpad_window("k2"), None);
                t.drag(pad(), window_at(5, 1200.).frame);
                assert_eq!(t.reactor.layout.scratchpad_window("k2"), Some(pad()));
            }

            #[test]
            fn window_moved_from_a_disabled_space_stays_registered() {
                let mut t = Test::with_screens(
                    vec![rule("com.testapp3", "l", Some(FRAME))],
                    vec![SCREEN, SCREEN2],
                    vec![Some(SpaceId::new(1)), None],
                    vec![],
                );
                t.launch(3, vec![window_at(7, 1200.)], false);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert!(t.reactor.layout.floating_windows_in_space(SpaceId::new(1)).is_empty());
                t.drag(launched_pad(), make_window(7).frame);
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert_eq!(
                    t.reactor.layout.floating_windows_in_space(SpaceId::new(1)),
                    [launched_pad()].into_iter().collect()
                );
            }

            #[test]
            fn first_matching_rule_decides_the_scratchpad() {
                let float = WindowRule {
                    conditions: WindowRuleConditions {
                        app_id: Some("com.testapp2".into()),
                        ..Default::default()
                    },
                    float: Some(true),
                    scratchpad: None,
                    frame: None,
                };
                let t = Test::with_rules(vec![rule("com.testapp2", "k", Some(FRAME)), float]);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(pad()));
            }

            #[test]
            fn window_list_updated_while_hidden_does_not_stop_the_next_hide() {
                let mut t = Test::new();
                t.hide_pad();
                t.focus_app(1);
                // Another app changed the windows on screen while the
                // scratchpad app was hidden.
                t.update_on_screen(|wsid| wsid != WindowServerId::new(5));
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(has_set_hidden(&requests, 2, false), "{requests:?}");

                // The window server reports the window again once the app is
                // shown.
                t.reactor.handle_event(Event::ApplicationHiddenChanged(2, false));
                t.update_on_screen(|_| true);
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(
                    matches!(requests[..], [(2, Request::SetHidden(true))]),
                    "{requests:?}"
                );
            }

            #[test]
            fn main_window_of_a_two_window_app_becomes_the_scratchpad() {
                for main in [WindowId::new(3, 1), WindowId::new(3, 2)] {
                    let mut t = Test::new();
                    let windows = vec![make_window(7), make_window(8)];
                    t.windows.extend(windows.iter().map(|w| (3, w.clone())));
                    let events = t.apps.make_app_with_opts(3, windows, Some(main), false);
                    t.reactor.handle_events(events);
                    t.settle();
                    assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(main));
                }
            }

            #[test]
            fn lowest_window_becomes_the_scratchpad_without_a_main_window() {
                let mut t = Test::new();
                let windows = vec![make_window(7), make_window(8), make_window(9)];
                t.windows.extend(windows.iter().map(|w| (3, w.clone())));
                let events = t.apps.make_app_with_opts(3, windows, None, false);
                t.reactor.handle_events(events);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
            }

            #[test]
            fn show_queued_outside_send_layout_event_is_taken_on_screen_change() {
                let mut t = Test::new();
                t.launch(3, vec![], false);
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                let window = make_window(7);
                t.reactor.windows.insert(launched_pad(), window.clone().into());
                let info = t.reactor.layout_window_info(launched_pad()).unwrap();
                _ = t.reactor.layout.handle_event(LayoutEvent::WindowAdded(
                    SpaceId::new(1),
                    launched_pad(),
                    info,
                ));
                assert!(t.focus_requests().is_empty());

                t.reactor.handle_event(Event::ScreenParametersChanged {
                    frames: vec![SCREEN],
                    spaces: vec![Some(SpaceId::new(1))],
                    scale_factors: vec![2.0],
                    converter: CoordinateConverter::default(),
                    on_screen: Default::default(),
                });
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            pub(super) fn frames_of(requests: &[(pid_t, Request)], wid: WindowId) -> Vec<CGRect> {
                requests
                    .iter()
                    .filter_map(|(_, req)| match req {
                        Request::SetWindowFrame(w, f, _) if *w == wid => Some(*f),
                        _ => None,
                    })
                    .collect()
            }

            #[test]
            fn second_toggle_before_the_window_appears_shows_it_once() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert!(!t.launches().is_empty());
                assert!(t.apps.tagged_requests().is_empty());
                assert!(t.focus_requests().is_empty());

                t.launch(3, vec![make_window(7)], false);
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
                let events = t.apps.simulate_events_for_tagged_requests(requests);
                t.reactor.handle_events(events);

                t.refresh(vec![Some(SpaceId::new(1))]);
                assert!(
                    !t.focus_requests().contains(&launched_pad()),
                    "the second toggle must not leave another pending show"
                );
            }

            #[test]
            fn launched_window_on_another_screen_is_placed_on_the_active_screen() {
                let mut t = Test::two_screens(make_window(5));
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.launch(3, vec![window_at(7, 1200.)], false);
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn toggle_brings_a_window_from_another_screen_to_the_active_screen() {
                let mut t = Test::two_screens(window_at(5, 1200.));
                t.focus_app(1);
                t.settle();
                assert_eq!(
                    t.reactor.layout.floating_windows_in_space(SpaceId::new(2)),
                    [pad()].into_iter().collect()
                );

                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert!(!has_set_hidden(&requests, 2, true), "{requests:?}");
                assert_eq!(t.focus_requests(), [pad()]);
                let events = t.apps.simulate_events_for_tagged_requests(requests);
                t.reactor.handle_events(events);

                // Now it is on the active screen and focused: hide it.
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(
                    matches!(requests[..], [(2, Request::SetHidden(true))]),
                    "{requests:?}"
                );
            }

            #[test]
            fn focused_window_on_the_other_screen_is_hidden() {
                // The active screen follows the focused window.
                let mut t = Test::two_screens(window_at(5, 1200.));
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(
                    matches!(requests[..], [(2, Request::SetHidden(true))]),
                    "{requests:?}"
                );
            }

            #[test]
            fn launched_window_on_a_disabled_space_is_shown() {
                let mut t = Test::with_screens(
                    vec![rule("com.testapp3", "l", Some(FRAME))],
                    vec![SCREEN, SCREEN2],
                    vec![Some(SpaceId::new(1)), None],
                    vec![],
                );
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert_eq!(t.launches(), ["com.testapp3"]);
                t.launch(3, vec![window_at(7, 1200.)], false);
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                let events = t.apps.simulate_events_for_tagged_requests(requests);
                t.reactor.handle_events(events);

                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert_eq!(t.launches(), ["com.testapp3"], "not launched again");
            }

            #[test]
            fn window_created_later_on_a_disabled_space_is_registered() {
                let mut t = Test::with_screens(
                    vec![rule("com.testapp3", "l", Some(FRAME))],
                    vec![SCREEN, SCREEN2],
                    vec![Some(SpaceId::new(1)), None],
                    vec![],
                );
                t.launch(3, vec![], false);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), None);
                t.add_window(launched_pad(), window_at(7, 1200.));
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert!(t.reactor.layout.floating_windows_in_space(SpaceId::new(1)).is_empty());

                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert!(t.launches().is_empty());
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn window_on_a_disabled_space_is_registered_and_shown_without_a_launch() {
                let mut t = Test::new();
                t.refresh(vec![None]);
                t.launch(3, vec![make_window(7)], false);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));

                t.focus_app(1);
                t.settle();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert!(t.launches().is_empty());
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad()]);
                assert!(
                    !requests.iter().any(|(pid, _)| *pid == 1),
                    "windows on the disabled space are left alone: {requests:?}"
                );
            }

            #[test]
            fn window_that_is_not_on_screen_is_registered() {
                let mut t = Test::new();
                // The app's window is on another space, but known from an
                // earlier launch event.
                let window = make_window(7);
                t.windows.push((3, window.clone()));
                let events = t.apps.make_app_with_opts(3, vec![window], None, false);
                t.reactor.handle_events(events);
                t.update_on_screen(|wsid| wsid != WindowServerId::new(7));
                t.reactor.handle_event(Event::WindowsDiscovered {
                    pid: 3,
                    new: vec![],
                    known_visible: vec![],
                });
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                assert!(t.launches().is_empty());
            }

            #[test]
            fn tiled_window_on_a_disabled_space_does_not_become_a_scratchpad() {
                let mut t = Test::new();
                // App 1 is tiled, then a rule makes it a scratchpad while its
                // space is disabled.
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp1",
                    "t",
                    None,
                )])));
                t.refresh(vec![None]);
                assert_eq!(t.reactor.layout.scratchpad_window("t"), None);
            }

            #[test]
            fn visible_window_is_preferred_over_a_window_on_another_space() {
                let mut t = Test::new();
                let windows = vec![make_window(7), make_window(8)];
                t.windows.extend(windows.iter().map(|w| (3, w.clone())));
                let events = t.apps.make_app_with_opts(3, windows, None, false);
                t.reactor.handle_events(events);
                t.update_on_screen(|wsid| wsid != WindowServerId::new(7));
                t.reactor.handle_event(Event::WindowDestroyed(launched_pad()));
                t.reactor.handle_event(Event::WindowsDiscovered {
                    pid: 3,
                    new: vec![(launched_pad(), make_window(7))],
                    known_visible: vec![],
                });
                assert_eq!(
                    t.reactor.layout.scratchpad_window("l"),
                    Some(WindowId::new(3, 2))
                );
            }

            #[test]
            fn main_window_on_another_screen_becomes_the_scratchpad() {
                let mut t = Test::two_screens(make_window(5));
                let main = WindowId::new(3, 2);
                let windows = vec![make_window(7), window_at(8, 1200.)];
                t.windows.extend(windows.iter().map(|w| (3, w.clone())));
                let events = t.apps.make_app_with_opts(3, windows, Some(main), false);
                t.reactor.handle_events(events);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(main));
                assert_eq!(
                    t.reactor.layout.floating_windows_in_space(SpaceId::new(2)),
                    [main].into_iter().collect()
                );
            }

            #[test]
            fn registered_scratchpad_is_shown_while_the_space_is_disabled() {
                let mut t = Test::new();
                t.refresh(vec![None]);
                t.focus_app(1);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(pad()));

                t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
                assert!(t.launches().is_empty());
                assert!(
                    !requests.iter().any(|(pid, _)| *pid == 1),
                    "windows on the disabled space are left alone: {requests:?}"
                );
            }

            #[test]
            fn only_the_scratchpad_window_of_a_two_window_app_is_shown() {
                let mut t = Test::with_screens(
                    vec![rule("com.testapp2", "k", Some(FRAME))],
                    vec![SCREEN],
                    vec![Some(SpaceId::new(1))],
                    vec![make_window(5), make_window(6)],
                );
                assert_eq!(
                    t.reactor.layout.floating_windows_in_space(SpaceId::new(1)),
                    [WindowId::new(2, 1), WindowId::new(2, 2)].into_iter().collect(),
                    "both windows match the rule and float"
                );
                // Either window can be the scratchpad; the other one is not.
                let pad = t.reactor.layout.scratchpad_window("k").unwrap();
                let other = [WindowId::new(2, 1), WindowId::new(2, 2)]
                    .into_iter()
                    .find(|&w| w != pad)
                    .unwrap();
                assert_eq!(t.reactor.layout.scratchpad_frame(other), None);

                // The other window of the scratchpad app is focused.
                t.focus_app(2);
                t.set_main_window(other);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad), [SHOWN]);
                assert!(frames_of(&requests, other).is_empty(), "{requests:?}");
                assert!(
                    !requests.iter().any(|(_, r)| matches!(r, Request::SetHidden(_))),
                    "{requests:?}"
                );
                assert_eq!(t.focus_requests(), [pad]);
                let events = t.apps.simulate_events_for_tagged_requests(requests);
                t.reactor.handle_events(events);

                t.set_main_window(pad);
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(
                    matches!(requests[..], [(2, Request::SetHidden(true))]),
                    "{requests:?}"
                );
            }

            #[test]
            fn remaining_window_of_the_app_takes_over_after_the_next_refresh() {
                let mut t = Test::with_screens(
                    vec![rule("com.testapp2", "k", Some(FRAME))],
                    vec![SCREEN],
                    vec![Some(SpaceId::new(1))],
                    vec![make_window(5), make_window(6)],
                );
                let pad = t.reactor.layout.scratchpad_window("k").unwrap();
                let second = [WindowId::new(2, 1), WindowId::new(2, 2)]
                    .into_iter()
                    .find(|&w| w != pad)
                    .unwrap();
                let pad_wsid = t.reactor.windows[&pad].window_server_id;
                t.reactor.handle_event(Event::WindowDestroyed(pad));
                t.apps.windows.remove(&pad);
                t.windows.retain(|(_, w)| w.sys_id != pad_wsid);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), None);

                t.refresh(vec![Some(SpaceId::new(1))]);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(second));
                t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                assert!(t.launches().is_empty());
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, second), [SHOWN]);
            }

            #[test]
            fn window_closed_while_hidden_is_replaced_by_a_launched_one() {
                let mut t = Test::new();
                t.hide_pad();
                t.focus_app(1);
                t.reactor.handle_event(Event::WindowDestroyed(pad()));
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("k"), None);
                assert!(t.reactor.is_app_hidden(2));

                t.reactor.handle_event(toggle("k", Some("com.testapp2")));
                assert_eq!(t.launches(), ["com.testapp2"]);
                assert!(t.apps.tagged_requests().is_empty());

                let new_pad = WindowId::new(2, 2);
                t.add_window(new_pad, make_window(8));
                let requests = t.apps.tagged_requests();
                assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
                assert_eq!(frames_of(&requests, new_pad), [SHOWN]);
                assert_eq!(t.focus_requests(), [new_pad]);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(new_pad));
            }

            #[test]
            fn window_closed_while_hidden_is_not_shown_by_the_next_window_without_toggle() {
                let mut t = Test::new();
                t.hide_pad();
                t.reactor.handle_event(Event::WindowDestroyed(pad()));
                t.settle();
                t.add_window(WindowId::new(2, 2), make_window(8));
                let requests = t.apps.tagged_requests();
                assert!(!has_set_hidden(&requests, 2, false), "{requests:?}");
                assert!(t.focus_requests().is_empty());
                assert_eq!(
                    t.reactor.layout.scratchpad_window("k"),
                    Some(WindowId::new(2, 2))
                );
            }

            #[test]
            fn renamed_rule_moves_the_window_to_the_new_name_after_reload() {
                let mut t = Test::new();
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp2",
                    "k2",
                    Some(FRAME),
                )])));
                t.refresh(vec![Some(SpaceId::new(1))]);
                assert_eq!(t.reactor.layout.scratchpad_window("k2"), Some(pad()));
                assert_eq!(t.reactor.layout.scratchpad_window("k"), None);
                assert!(t.focus_requests().is_empty(), "reload shows nothing");

                t.reactor.handle_event(toggle("k2", Some("com.testapp2")));
                assert!(t.launches().is_empty());
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn changed_frame_is_used_after_reload() {
                let mut t = Test::new();
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp2",
                    "k",
                    None,
                )])));
                t.refresh(vec![Some(SpaceId::new(1))]);
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert_eq!(
                    frames_of(&requests, pad()),
                    [FractionalRect::DEFAULT.to_frame(SCREEN)]
                );
            }

            #[test]
            fn removed_rule_stops_the_window_being_a_scratchpad_after_reload() {
                let mut t = Test::new();
                t.reactor.handle_event(Event::ConfigChanged(config(vec![])));
                t.refresh(vec![Some(SpaceId::new(1))]);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), None);

                // The window can now be tiled like any other floating window.
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(Event::Command(Command::Layout(
                    LayoutCommand::ToggleWindowFloating,
                )));
                t.settle();
                assert!(t.reactor.layout.floating_windows_in_space(SpaceId::new(1)).is_empty());
            }

            #[test]
            fn hiding_another_app_does_not_change_the_scratchpad_decision() {
                let mut t = Test::new();
                t.focus_app(2);
                t.reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(
                    matches!(requests[..], [(2, Request::SetHidden(true))]),
                    "{requests:?}"
                );
                let events = t.apps.simulate_events_for_tagged_requests(requests);
                t.reactor.handle_events(events);
                assert!(t.reactor.is_app_hidden(1));
                assert!(t.reactor.is_app_hidden(2));

                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
                assert!(
                    !requests.iter().any(|(pid, _)| *pid == 1),
                    "the other hidden app stays hidden: {requests:?}"
                );
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn move_focus_does_nothing_from_a_visible_scratchpad() {
                let mut t = Test::new();
                t.focus_app(2);
                t.settle();
                for direction in [Direction::Left, Direction::Right] {
                    t.reactor.handle_event(Event::Command(Command::Layout(
                        LayoutCommand::MoveFocus(direction),
                    )));
                }
                assert!(t.focus_requests().is_empty());
                assert!(t.apps.tagged_requests().is_empty());
            }

            #[test]
            fn toggle_focus_floating_leaves_a_visible_scratchpad_for_the_tiled_windows() {
                let mut t = Test::new();
                t.focus_app(2);
                t.settle();
                t.reactor.handle_event(Event::Command(Command::Layout(
                    LayoutCommand::ToggleFocusFloating,
                )));
                let focus = t.focus_requests();
                assert_eq!(focus.len(), 1, "{focus:?}");
                assert_eq!(focus[0].pid, 1, "{focus:?}");
                assert!(t.apps.tagged_requests().is_empty());

                // Back from the tiled window: the scratchpad is not the
                // floating window to return to.
                t.focus_app(1);
                t.settle();
                t.reactor.handle_event(Event::Command(Command::Layout(
                    LayoutCommand::ToggleFocusFloating,
                )));
                assert!(t.focus_requests().is_empty());
            }

            fn titled_rule(app_id: &str, title: &str, name: &str) -> WindowRule {
                let mut rule = rule(app_id, name, Some(FRAME));
                rule.conditions.title_substring = Some(title.into());
                rule
            }

            #[test]
            fn rule_moved_to_another_app_releases_the_old_window() {
                let mut t = Test::new();
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp3",
                    "k",
                    Some(FRAME),
                )])));
                t.refresh(vec![Some(SpaceId::new(1))]);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), None);

                t.launch(3, vec![make_window(7)], false);
                t.settle();
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(launched_pad()));
            }

            #[test]
            fn every_scratchpad_found_by_one_launch_is_shown_at_once() {
                let mut t = Test::with_rules(vec![
                    titled_rule("com.testapp3", "Window7", "l"),
                    titled_rule("com.testapp3", "Window8", "m"),
                ]);
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(toggle("m", Some("com.testapp3")));
                let second = WindowId::new(3, 2);

                let windows = vec![make_window(7), make_window(8)];
                t.windows.extend(windows.iter().map(|w| (3, w.clone())));
                let events = t.apps.make_app_with_opts(3, windows, None, false);
                t.reactor.handle_events(events);
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert_eq!(t.reactor.layout.scratchpad_window("m"), Some(second));
                let requests = t.apps.tagged_requests();
                assert_eq!(frames_of(&requests, launched_pad()), [SHOWN]);
                assert_eq!(frames_of(&requests, second), [SHOWN]);
                assert_eq!(t.focus_requests(), [launched_pad(), second]);
            }

            #[test]
            fn expiry_of_an_unknown_show_changes_nothing() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                let id = t.last_launch_id();
                t.reactor.handle_event(toggle("k", None));
                t.settle();
                t.reactor.handle_event(Event::ScratchpadShowExpired(id));
                t.reactor.handle_event(Event::ScratchpadShowExpired(id));
                assert!(t.apps.tagged_requests().is_empty());
                assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), None);
                assert_eq!(t.reactor.layout.scratchpad_window("k"), Some(pad()));
            }

            #[test]
            fn toggle_after_an_expired_show_launches_and_shows_again() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                let first = t.last_launch_id();
                t.reactor.handle_event(Event::ScratchpadShowExpired(first));
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                let second = t.last_launch_id();
                assert_ne!(first, second);
                assert_eq!(t.launches(), ["com.testapp3", "com.testapp3"]);
                assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), Some(second));

                t.launch(3, vec![make_window(7)], false);
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn quitting_another_app_keeps_the_pending_show() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(Event::ApplicationTerminated(2));
                t.reactor.handle_event(Event::ApplicationThreadTerminated(2));
                t.settle();
                assert!(t.reactor.layout.pending_scratchpad_show("l").is_some());

                t.launch(3, vec![make_window(7)], false);
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn app_quitting_before_its_window_appears_is_not_shown_on_relaunch() {
                let mut t = Test::new();
                t.launch(3, vec![], false);
                t.settle();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(Event::ApplicationTerminated(3));
                t.reactor.handle_event(Event::ApplicationThreadTerminated(3));
                t.settle();

                // The app is opened again by other means.
                t.launch(3, vec![make_window(7)], false);
                assert_eq!(t.reactor.layout.scratchpad_window("l"), Some(launched_pad()));
                assert!(t.focus_requests().is_empty());
                assert!(!has_frame(&t.apps.tagged_requests(), launched_pad(), SHOWN));
            }

            #[test]
            fn pending_show_survives_a_reload_that_keeps_the_rule() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp3",
                    "l",
                    None,
                )])));
                t.refresh(vec![Some(SpaceId::new(1))]);
                t.launch(3, vec![make_window(7)], false);
                let requests = t.apps.tagged_requests();
                assert_eq!(
                    frames_of(&requests, launched_pad()),
                    [FractionalRect::DEFAULT.to_frame(SCREEN)]
                );
                assert_eq!(t.focus_requests(), [launched_pad()]);
            }

            #[test]
            fn pending_show_of_a_renamed_rule_is_dropped() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(Event::ConfigChanged(config(vec![rule(
                    "com.testapp3",
                    "l2",
                    Some(FRAME),
                )])));
                assert_eq!(t.reactor.layout.pending_scratchpad_show("l"), None);
                t.launch(3, vec![make_window(7)], false);
                assert_eq!(t.reactor.layout.scratchpad_window("l2"), Some(launched_pad()));
                assert!(t.focus_requests().is_empty());
                assert!(!has_frame(&t.apps.tagged_requests(), launched_pad(), SHOWN));
            }

            #[test]
            fn app_hidden_by_the_system_updates_the_next_toggle() {
                let mut t = Test::new();
                t.focus_app(2);
                t.settle();
                // Cmd+H on the scratchpad app: the app actor reports the
                // change, then the window server drops the window.
                t.reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
                t.update_on_screen(|wsid| wsid != WindowServerId::new(5));
                t.focus_app(1);
                t.settle();
                t.reactor.handle_event(toggle("k", None));
                let requests = t.apps.tagged_requests();
                assert!(has_set_hidden(&requests, 2, false), "{requests:?}");
                assert_eq!(frames_of(&requests, pad()), [SHOWN]);
                assert_eq!(t.focus_requests(), [pad()]);
            }

            #[test]
            fn launch_ids_are_the_same_on_replay() {
                let mut t = Test::new();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                let first = t.last_launch_id();
                t.reactor.handle_event(toggle("l", Some("com.testapp3")));
                t.reactor.handle_event(toggle("m", Some("com.testapp4")));
                t.reactor.handle_event(Event::ScratchpadShowExpired(first));
                let trace = replay::tests::recorded_trace(&mut t.reactor);
                let replayed = replay::tests::replay_trace(&trace);
                assert_eq!(
                    replayed.layout.pending_scratchpad_show("l"),
                    t.reactor.layout.pending_scratchpad_show("l")
                );
                assert_eq!(
                    replayed.layout.pending_scratchpad_show("m"),
                    t.reactor.layout.pending_scratchpad_show("m")
                );
                assert!(replayed.layout.pending_scratchpad_show("l").is_some());
            }
        }
    }

    fn reactor_with_one_screen() -> Reactor {
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor
    }

    #[test]
    fn app_hidden_state_follows_hidden_changed_events() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        assert!(!reactor.is_app_hidden(1));

        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        assert!(reactor.is_app_hidden(1));
        reactor.handle_event(Event::ApplicationHiddenChanged(1, false));
        assert!(!reactor.is_app_hidden(1));

        assert!(!reactor.is_app_hidden(2));
        reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
        assert!(!reactor.is_app_hidden(2), "unknown apps are not tracked");
        assert!(apps.requests().is_empty());
    }

    #[test]
    fn app_hidden_state_starts_from_launch_info() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        let mut events = apps.make_app(1, make_windows(1));
        for event in &mut events {
            if let Event::ApplicationLaunched { info, .. } = event {
                info.is_hidden = true;
            }
        }
        reactor.handle_events(events);
        reactor.handle_events(apps.make_app(2, make_windows(0)));
        assert!(reactor.is_app_hidden(1));
        assert!(!reactor.is_app_hidden(2));

        reactor.handle_event(Event::ApplicationThreadTerminated(1));
        assert!(!reactor.is_app_hidden(1));
    }

    #[test]
    fn harness_answers_set_hidden_with_hidden_changed() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_events(apps.make_app(2, make_windows(0)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        reactor.apps[&2].handle.send(Request::SetHidden(true)).unwrap();
        let requests = apps.tagged_requests();
        assert!(
            matches!(requests[..], [(2, Request::SetHidden(true))]),
            "{requests:?}"
        );
        let events = apps.simulate_events_for_tagged_requests(requests);
        assert!(matches!(events[..], [Event::ApplicationHiddenChanged(2, true)]));
        reactor.handle_events(events);
        assert_eq!(apps.hidden.get(&2), Some(&true));
        assert!(reactor.is_app_hidden(2));
        assert!(!reactor.is_app_hidden(1));

        reactor.apps[&2].handle.send(Request::SetHidden(false)).unwrap();
        apps.simulate_until_quiet(&mut reactor);
        assert_eq!(apps.hidden.get(&2), Some(&false));
        assert!(!reactor.is_app_hidden(2));
    }

    #[test]
    fn app_info_without_hidden_field_deserializes_as_shown() {
        let info: AppInfo =
            ron::de::from_str(r#"(bundle_id: Some("com.example"), localized_name: None)"#).unwrap();
        assert!(!info.is_hidden);
        let event = Event::ApplicationHiddenChanged(3, true);
        let line = ron::ser::to_string(&event).unwrap();
        let back: Event = ron::de::from_str(&line).unwrap();
        assert!(matches!(back, Event::ApplicationHiddenChanged(3, true)));
    }

    fn launch_events(apps: &mut Apps, pid: pid_t, windows: usize, hidden: bool) -> Vec<Event> {
        let mut events = apps.make_app(pid, make_windows(windows));
        for event in &mut events {
            if let Event::ApplicationLaunched { info, .. } = event {
                info.is_hidden = hidden;
            }
        }
        events
    }

    fn window_frames(reactor: &Reactor) -> Vec<(WindowId, CGRect)> {
        reactor
            .windows
            .iter()
            .map(|(&wid, w)| (wid, w.frame_monotonic))
            .sorted_by_key(|(wid, _)| *wid)
            .collect()
    }

    #[test]
    fn repeated_hidden_changed_events_keep_the_last_state() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        let frames = window_frames(&reactor);

        for _ in 0..2 {
            reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
            assert!(reactor.is_app_hidden(1));
        }
        for _ in 0..2 {
            reactor.handle_event(Event::ApplicationHiddenChanged(1, false));
            assert!(!reactor.is_app_hidden(1));
        }
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        assert!(reactor.is_app_hidden(1));

        let requests = apps.tagged_requests();
        assert!(
            requests.is_empty(),
            "hide/show must not produce requests: {requests:?}"
        );
        assert_eq!(
            window_frames(&reactor),
            frames,
            "hide/show must not change the layout"
        );
    }

    #[test]
    fn hidden_state_is_tracked_per_app() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(launch_events(&mut apps, 1, 1, false));
        reactor.handle_events(launch_events(&mut apps, 2, 1, true));
        reactor.handle_events(launch_events(&mut apps, 3, 0, false));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        assert_eq!(
            [1, 2, 3].map(|pid| reactor.is_app_hidden(pid)),
            [false, true, false]
        );

        reactor.handle_event(Event::ApplicationHiddenChanged(3, true));
        reactor.handle_event(Event::ApplicationHiddenChanged(2, false));
        assert_eq!(
            [1, 2, 3].map(|pid| reactor.is_app_hidden(pid)),
            [false, false, true]
        );
    }

    #[test]
    fn hidden_state_survives_application_terminated_until_thread_exits() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));

        reactor.handle_event(Event::ApplicationTerminated(1));
        assert!(reactor.is_app_hidden(1));
        let requests = apps.tagged_requests();
        assert!(matches!(requests[..], [(1, Request::Terminate)]), "{requests:?}");

        reactor.handle_event(Event::ApplicationThreadTerminated(1));
        assert!(!reactor.is_app_hidden(1));
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        assert!(
            !reactor.is_app_hidden(1),
            "a late event for a terminated app must not resurrect its state"
        );
    }

    #[test]
    fn relaunched_app_with_same_pid_starts_from_new_launch_info() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(launch_events(&mut apps, 1, 1, false));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        reactor.handle_event(Event::ApplicationThreadTerminated(1));
        apps.simulate_until_quiet(&mut reactor);

        reactor.handle_events(launch_events(&mut apps, 1, 1, false));
        apps.simulate_until_quiet(&mut reactor);
        assert!(!reactor.is_app_hidden(1));
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        assert!(reactor.is_app_hidden(1));

        reactor.handle_event(Event::ApplicationThreadTerminated(1));
        apps.simulate_until_quiet(&mut reactor);
        reactor.handle_events(launch_events(&mut apps, 1, 1, true));
        apps.simulate_until_quiet(&mut reactor);
        assert!(reactor.is_app_hidden(1));
    }

    #[test]
    fn relaunched_app_with_new_pid_is_independent_of_old_pid() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(launch_events(&mut apps, 1, 1, false));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        reactor.handle_event(Event::ApplicationThreadTerminated(1));
        apps.simulate_until_quiet(&mut reactor);

        reactor.handle_events(launch_events(&mut apps, 5, 1, false));
        apps.simulate_until_quiet(&mut reactor);
        assert!(!reactor.is_app_hidden(5));
        assert!(!reactor.is_app_hidden(1));

        reactor.handle_event(Event::ApplicationHiddenChanged(1, true));
        assert!(!reactor.is_app_hidden(5));
        assert!(!reactor.is_app_hidden(1));
        reactor.handle_event(Event::ApplicationHiddenChanged(5, true));
        assert!(reactor.is_app_hidden(5));
    }

    #[test]
    fn harness_attributes_set_hidden_to_the_app_it_was_sent_to() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        for pid in [1, 2, 3] {
            reactor.handle_events(apps.make_app(pid, make_windows(1)));
        }
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        reactor.apps[&3].handle.send(Request::SetHidden(true)).unwrap();
        reactor.apps[&1].handle.send(Request::SetHidden(true)).unwrap();
        reactor.apps[&3].handle.send(Request::SetHidden(false)).unwrap();
        let requests = apps.tagged_requests();
        assert!(
            matches!(
                requests[..],
                [
                    (1, Request::SetHidden(true)),
                    (3, Request::SetHidden(true)),
                    (3, Request::SetHidden(false)),
                ]
            ),
            "{requests:?}"
        );
        let events = apps.simulate_events_for_tagged_requests(requests);
        assert!(
            matches!(
                events[..],
                [
                    Event::ApplicationHiddenChanged(1, true),
                    Event::ApplicationHiddenChanged(3, true),
                    Event::ApplicationHiddenChanged(3, false),
                ]
            ),
            "{events:?}"
        );
        reactor.handle_events(events);
        assert_eq!(apps.hidden, BTreeMap::from([(1, true), (3, false)]));
        assert_eq!(
            [1, 2, 3].map(|pid| reactor.is_app_hidden(pid)),
            [true, false, false]
        );
    }

    #[test]
    fn harness_simulate_events_answers_set_hidden() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_events(apps.make_app(2, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        reactor.apps[&2].handle.send(Request::SetHidden(true)).unwrap();
        let events = apps.simulate_events();
        assert!(
            matches!(events[..], [Event::ApplicationHiddenChanged(2, true)]),
            "{events:?}"
        );
        assert!(apps.simulate_events().is_empty(), "requests are consumed once");
    }

    #[test]
    fn harness_untagged_requests_still_see_every_app() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_events(apps.make_app(2, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        reactor.apps[&2].handle.send(Request::SetHidden(false)).unwrap();
        reactor.apps[&1].handle.send(Request::GetVisibleWindows).unwrap();
        let requests = apps.requests();
        assert!(
            matches!(
                requests[..],
                [Request::GetVisibleWindows, Request::SetHidden(false)]
            ),
            "{requests:?}"
        );
        assert!(apps.requests().is_empty());
    }

    #[test]
    fn harness_terminate_only_drops_requests_of_the_terminated_app() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_events(apps.make_app(2, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        reactor.apps[&1].handle.send(Request::Terminate).unwrap();
        reactor.apps[&1].handle.send(Request::SetHidden(true)).unwrap();
        reactor.apps[&2].handle.send(Request::SetHidden(true)).unwrap();
        let events = apps.simulate_events();
        assert!(
            matches!(events[..], [Event::ApplicationHiddenChanged(2, true)]),
            "{events:?}"
        );
    }

    #[test]
    #[should_panic(expected = "SetHidden needs the app pid")]
    fn harness_untagged_simulation_rejects_set_hidden() {
        let mut apps = Apps::new();
        apps.simulate_events_for_requests(vec![Request::SetHidden(true)]);
    }

    #[test]
    fn hidden_changed_event_is_recorded_and_replayed() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(launch_events(&mut apps, 1, 1, true));
        reactor.handle_events(apps.make_app(2, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        reactor.handle_event(Event::ApplicationHiddenChanged(2, true));
        reactor.handle_event(Event::ApplicationHiddenChanged(1, false));

        let trace = replay::tests::recorded_trace(&mut reactor);
        assert!(trace.contains("ApplicationHiddenChanged(2,true)"), "{trace}");
        assert!(trace.contains("is_hidden:true"), "{trace}");
        let replayed = replay::tests::replay_trace(&trace);
        assert_eq!([1, 2].map(|pid| replayed.is_app_hidden(pid)), [false, true]);
    }

    #[test]
    fn old_trace_without_hidden_field_replays_as_shown() {
        let mut apps = Apps::new();
        let mut reactor = reactor_with_one_screen();
        reactor.handle_events(launch_events(&mut apps, 1, 1, true));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        let trace = replay::tests::recorded_trace(&mut reactor);
        let old_trace = trace.replace(",is_hidden:true", "");
        assert_ne!(old_trace, trace, "the launch info must carry is_hidden: {trace}");
        assert!(!old_trace.contains("is_hidden"));
        let replayed = replay::tests::replay_trace(&old_trace);
        assert!(replayed.apps.contains_key(&1));
        assert!(!replayed.is_app_hidden(1));
        let replayed = replay::tests::replay_trace(&trace);
        assert!(replayed.is_app_hidden(1));
    }

    #[test]
    fn floating_window_restores_its_last_user_frame() {
        use LayoutCommand::ToggleWindowFloating;

        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let wid = WindowId::new(1, 1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(1), Some(wid), true));
        reactor.handle_event(Event::ApplicationGloballyActivated(1));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);
        reactor.send_layout_event(LayoutEvent::WindowFocused(vec![space], wid));

        // First float restores the frame the window had before it was tiled.
        reactor.handle_event(Event::Command(Command::Layout(ToggleWindowFloating)));
        let initial_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(50., 50.));
        let requests = apps.requests();
        assert!(requests.iter().any(|request| {
            matches!(request, Request::SetWindowFrame(request_wid, frame, _) if *request_wid == wid && *frame == initial_frame)
        }), "{requests:?}");
        for event in apps.simulate_events_for_requests(requests) {
            reactor.handle_event(event);
        }

        // A user move/resize while floating becomes the next restore target.
        let updated_frame = CGRect::new(CGPoint::new(300., 400.), CGSize::new(250., 125.));
        reactor.handle_event(Event::WindowFrameChanged(
            wid,
            updated_frame,
            apps.windows[&wid].last_seen_txid,
            Requested(false),
            None,
        ));
        assert_eq!(reactor.layout.floating_restore_frame(wid), Some(updated_frame));
        assert!(apps.requests().is_empty());

        reactor.handle_event(Event::Command(Command::Layout(ToggleWindowFloating)));
        apps.simulate_until_quiet(&mut reactor);
        assert_eq!(reactor.windows[&wid].frame_monotonic, screen);
        reactor.handle_event(Event::Command(Command::Layout(ToggleWindowFloating)));
        let requests = apps.requests();
        assert!(requests.iter().any(|request| {
            matches!(request, Request::SetWindowFrame(request_wid, frame, _) if *request_wid == wid && *frame == updated_frame)
        }), "{requests:?}");
    }

    #[test]
    fn floating_frame_restoration_uses_animation() {
        use LayoutCommand::ToggleWindowFloating;

        let mut apps = Apps::new();
        let (mut reactor, mut animation_rx) =
            Reactor::new_for_test_with_animation(LayoutManager::new_for_test(), true);
        let space = SpaceId::new(1);
        let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let wid = WindowId::new(1, 1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(1), Some(wid), true));
        reactor.handle_event(Event::ApplicationGloballyActivated(1));
        reactor.handle_event(Event::StartupComplete);
        let animation::Message::Replace(animation) = animation_rx.try_recv().unwrap() else {
            panic!("expected initial layout animation");
        };
        animation.skip_to_end();
        apps.simulate_until_quiet(&mut reactor);
        reactor.send_layout_event(LayoutEvent::WindowFocused(vec![space], wid));

        reactor.handle_event(Event::Command(Command::Layout(ToggleWindowFloating)));
        assert!(
            apps.requests().is_empty(),
            "restore should be animated, not written directly"
        );
        assert!(matches!(
            animation_rx.try_recv(),
            Ok(animation::Message::Replace(_))
        ));
    }

    #[test]
    fn it_sends_writes_when_stale_read_state_looks_same_as_written_state() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        let events_1 = apps.simulate_events();
        let state_1 = apps.windows.clone();
        assert!(!state_1.is_empty());

        for event in events_1 {
            reactor.handle_event(event);
        }
        assert!(apps.requests().is_empty());

        reactor.handle_events(apps.make_app(2, make_windows(1)));
        let _events_2 = apps.simulate_events();

        reactor.handle_event(Event::WindowDestroyed(WindowId::new(2, 1)));
        let _events_3 = apps.simulate_events();
        let state_3 = apps.windows;

        // These should be the same, because we should have resized the first
        // two windows both at the beginning, and at the end when the third
        // window was destroyed.
        for (wid, state) in dbg!(state_1) {
            assert!(state_3.contains_key(&wid), "{wid:?} not in {state_3:#?}");
            assert_eq!(state.frame, state_3[&wid].frame);
        }
    }

    #[test]
    fn sends_writes_same_as_last_written_state_if_changed_externally() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        let events_1 = apps.simulate_events();
        let state_1 = apps.windows.clone();
        assert!(!state_1.is_empty());

        for event in events_1 {
            reactor.handle_event(event);
        }
        assert!(apps.requests().is_empty());

        // Move a window in an invalid way.
        let wid = WindowId::new(1, 1);
        let old_frame = state_1[&wid].frame;
        reactor.handle_event(Event::WindowFrameChanged(
            wid,
            CGRect::new(
                CGPoint::new(old_frame.origin.x, old_frame.origin.y + 10.),
                old_frame.size,
            ),
            state_1[&wid].last_seen_txid,
            Requested(false),
            None,
        ));

        let requests = apps.requests();
        assert!(!requests.is_empty());
        let _events_2 = apps.simulate_events_for_requests(requests);
        assert_eq!(apps.windows[&wid].frame, old_frame);
    }

    #[test]
    fn it_responds_to_resizes() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(3)));
        reactor.handle_event(Event::StartupComplete);

        let events = apps.simulate_events();
        let windows = apps.windows.clone();
        for event in events {
            reactor.handle_event(event);
        }
        assert!(
            apps.requests().is_empty(),
            "reactor shouldn't react to unsurprising events"
        );

        // Resize the right edge of the middle window.
        let resizing = WindowId::new(1, 2);
        let window = &apps.windows[&resizing];
        let frame = CGRect::new(
            window.frame.origin,
            CGSize::new(window.frame.size.width + 10., window.frame.size.height),
        );
        reactor.handle_event(Event::WindowFrameChanged(
            resizing,
            frame,
            window.last_seen_txid,
            Requested(false),
            None,
        ));

        // Expect the next window to be resized.
        let next = WindowId::new(1, 3);
        let old_frame = windows[&next].frame;
        let requests = apps.requests();
        assert!(!requests.is_empty());
        let _events = apps.simulate_events_for_requests(requests);
        assert_ne!(old_frame, apps.windows[&next].frame);
    }

    #[test]
    fn it_manages_windows_on_enabled_spaces() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);

        let _events = apps.simulate_events();
        assert_eq!(
            full_screen,
            apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
        );
    }

    #[test]
    fn it_stops_writing_a_frame_the_app_keeps_undoing() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![1.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        // The app moves the window off its tiled position after every write.
        let wid = WindowId::new(1, 1);
        let moved_back = CGRect::new(CGPoint::new(100., 100.), full_screen.size);
        let mut writes = 0;
        let mut writes_in_last_round = 0;
        for _ in 0..MAX_FRAME_ATTEMPTS + 5 {
            reactor.handle_event(Event::WindowFrameChanged(
                wid,
                moved_back,
                apps.windows[&wid].last_seen_txid,
                Requested(false),
                None,
            ));
            let requests = apps.requests();
            writes_in_last_round = requests
                .iter()
                .filter(|request| {
                    matches!(
                        request,
                        Request::SetWindowFrame(..) | Request::AnimationFrame { .. }
                    )
                })
                .count();
            writes += writes_in_last_round;
            _ = apps.simulate_events_for_requests(requests);
        }
        // The initial placement counts toward the limit.
        assert!(writes <= MAX_FRAME_ATTEMPTS as usize, "{writes} writes");
        assert_eq!(0, writes_in_last_round);
    }

    #[test]
    fn windows_parked_off_screen_belong_to_no_screen() {
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        let off_screen = CGRect::new(CGPoint::new(0., -2000.), CGSize::new(1000., 500.));
        assert_eq!(None, reactor.best_screen_idx_for_window(&off_screen));

        let partly_on_screen = CGRect::new(CGPoint::new(0., -100.), CGSize::new(1000., 500.));
        assert_eq!(Some(0), reactor.best_screen_idx_for_window(&partly_on_screen));

        let no_area = CGRect::new(CGPoint::new(500., 500.), CGSize::new(0., 0.));
        assert_eq!(Some(0), reactor.best_screen_idx_for_window(&no_area));
    }

    #[test]
    fn it_selects_the_main_window_on_space_enable() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let ws_info = (1..=2)
            .map(|id| WindowServerInfo {
                id: WindowServerId::new(id),
                pid: 1,
                layer: 0,
                frame: CGRect::ZERO,
            })
            .collect::<Vec<_>>();
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![None],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(Event::WindowsOnScreenUpdated {
            pid: None,
            on_screen: WindowsOnScreen::new(ws_info.clone()),
        });

        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
        ));
        reactor.handle_event(Event::StartupComplete);
        reactor.handle_event(Event::ApplicationGloballyActivated(1));
        reactor.handle_events(apps.simulate_events());

        reactor.handle_event(Event::SpaceChanged(
            vec![Some(SpaceId::new(1))],
            WindowsOnScreen::new(ws_info),
        ));
        reactor.handle_events(apps.simulate_events());
        assert_eq!(
            reactor.layout.selected_window(SpaceId::new(1)),
            Some(WindowId::new(1, 1))
        );
    }

    #[test]
    fn it_surfaces_on_screen_change_when_the_snapshot_is_empty() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 900.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        assert!(raise_manager_rx.try_recv().is_ok());
    }

    #[test]
    fn it_surfaces_on_screen_change_when_the_snapshot_omits_the_windows() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        // The snapshot lists only a window we don't manage, so it says nothing
        // about where the windows we want to raise are.
        let on_screen = vec![WindowServerInfo {
            id: WindowServerId::new(999),
            pid: 2,
            layer: 0,
            frame: CGRect::new(CGPoint::new(0., 0.), CGSize::new(100., 100.)),
        }];
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 900.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: WindowsOnScreen::new(on_screen),
        });
        assert!(raise_manager_rx.try_recv().is_ok());
    }

    #[test]
    fn it_skips_surface_on_screen_change_when_top_layer_order_matches() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        // A screen change that doesn't disturb the stacking shouldn't restack.
        let desired = reactor
            .layout
            .handle_event(LayoutEvent::SpaceExposed(space, CGSize::new(1000., 900.)))
            .raise_windows;
        let on_screen = desired
            .iter()
            .map(|wid| WindowServerInfo {
                id: reactor.windows[wid].window_server_id.unwrap(),
                pid: wid.pid,
                layer: 0,
                frame: reactor.windows[wid].frame_monotonic,
            })
            .collect();
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 900.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: WindowsOnScreen::new(on_screen),
        });
        assert!(raise_manager_rx.try_recv().is_err());
    }

    #[test]
    fn it_surfaces_on_screen_change_when_another_window_is_in_front() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        // An unmanaged window is in front of the ones the layout wants on top,
        // so the restack is still needed.
        let mut on_screen = vec![WindowServerInfo {
            id: WindowServerId::new(999),
            pid: 2,
            layer: 0,
            frame: CGRect::new(CGPoint::new(0., 0.), CGSize::new(100., 100.)),
        }];
        on_screen.extend(reactor.windows.iter().map(|(wid, window)| WindowServerInfo {
            id: window.window_server_id.unwrap(),
            pid: wid.pid,
            layer: 0,
            frame: window.frame_monotonic,
        }));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 900.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: WindowsOnScreen::new(on_screen),
        });
        assert!(raise_manager_rx.try_recv().is_ok());
    }

    #[test]
    fn it_skips_surface_when_top_layer_order_matches() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        let desired = reactor
            .layout
            .handle_event(LayoutEvent::SpaceExposed(space, CGSize::new(1000., 1000.)))
            .raise_windows;
        let on_screen = desired
            .iter()
            .map(|wid| WindowServerInfo {
                id: reactor.windows[wid].window_server_id.unwrap(),
                pid: wid.pid,
                layer: 0,
                frame: reactor.windows[wid].frame_monotonic,
            })
            .collect();
        reactor.handle_event(Event::SpaceChanged(
            vec![Some(space)],
            WindowsOnScreen::new(on_screen),
        ));

        assert!(raise_manager_rx.try_recv().is_err());
    }

    #[test]
    fn it_skips_surface_when_top_layer_windows_are_reordered() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        let desired = reactor
            .layout
            .handle_event(LayoutEvent::SpaceExposed(space, CGSize::new(1000., 1000.)))
            .raise_windows;
        let on_screen = desired
            .iter()
            .rev()
            .map(|wid| WindowServerInfo {
                id: reactor.windows[wid].window_server_id.unwrap(),
                pid: wid.pid,
                layer: 0,
                frame: reactor.windows[wid].frame_monotonic,
            })
            .collect();
        reactor.handle_event(Event::SpaceChanged(
            vec![Some(space)],
            WindowsOnScreen::new(on_screen),
        ));

        assert!(raise_manager_rx.try_recv().is_err());
    }

    #[test]
    fn it_surfaces_top_layer_windows_when_top_managed_set_differs() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        let desired = reactor
            .layout
            .handle_event(LayoutEvent::SpaceExposed(space, CGSize::new(1000., 1000.)))
            .raise_windows;
        let mut on_screen = vec![WindowServerInfo {
            id: WindowServerId::new(90),
            pid: 9,
            layer: 0,
            frame: CGRect::new(CGPoint::new(10., 10.), CGSize::new(100., 100.)),
        }];
        on_screen.extend(desired.iter().map(|wid| WindowServerInfo {
            id: reactor.windows[wid].window_server_id.unwrap(),
            pid: wid.pid,
            layer: 0,
            frame: reactor.windows[wid].frame_monotonic,
        }));
        reactor.handle_event(Event::SpaceChanged(
            vec![Some(space)],
            WindowsOnScreen::new(on_screen),
        ));

        let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
        match msg {
            raise::Event::RaiseRequest(RaiseRequest {
                raise_windows,
                focus_window,
                app_handles: _,
            }) => {
                assert_eq!(raise_windows, vec![desired]);
                assert!(focus_window.is_none());
            }
            _ => panic!("Unexpected event: {msg:?}"),
        }
    }

    #[test]
    fn filter_response_clears_matching_focus_and_raise_windows() {
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.screens = vec![Screen {
            frame: CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.)),
            space: Some(SpaceId::new(1)),
            scale_factor: 2.0,
        }];
        let w1 = WindowId::with_wsid(1, WindowServerId::new(1));
        let w2 = WindowId::with_wsid(1, WindowServerId::new(2));
        reactor.windows.insert(
            w1,
            super::WindowState {
                title: Secret::new(String::new()),
                window_server_id: Some(WindowServerId::new(1)),
                frame_monotonic: CGRect::new(CGPoint::new(0., 0.), CGSize::new(500., 1000.)),
                is_ax_standard: true,
                is_resizable: true,
                ax_role: String::new(),
                ax_subrole: None,
                last_sent_txid: TransactionId::default(),
            },
        );
        reactor.windows.insert(
            w2,
            super::WindowState {
                title: Secret::new(String::new()),
                window_server_id: Some(WindowServerId::new(2)),
                frame_monotonic: CGRect::new(CGPoint::new(500., 0.), CGSize::new(500., 1000.)),
                is_ax_standard: true,
                is_resizable: true,
                ax_role: String::new(),
                ax_subrole: None,
                last_sent_txid: TransactionId::default(),
            },
        );
        reactor.visible_windows =
            [WindowServerId::new(1), WindowServerId::new(2)].into_iter().collect();
        reactor.window_server_info.insert(
            WindowServerId::new(1),
            WindowServerInfo {
                id: WindowServerId::new(1),
                pid: 1,
                layer: 0,
                frame: reactor.windows[&w1].frame_monotonic,
            },
        );
        reactor.window_server_info.insert(
            WindowServerId::new(2),
            WindowServerInfo {
                id: WindowServerId::new(2),
                pid: 1,
                layer: 0,
                frame: reactor.windows[&w2].frame_monotonic,
            },
        );

        let response = reactor.filter_response(
            layout::EventResponse {
                frame_overrides: vec![],
                raise_windows: vec![w2],
                focus_window: Some(w1),
            },
            &[WindowServerId::new(1), WindowServerId::new(2)],
        );

        assert!(response.raise_windows.is_empty());
        assert!(response.focus_window.is_none());
    }

    #[test]
    fn filter_response_keeps_response_when_focus_is_not_frontmost() {
        let reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let w1 = WindowId::with_wsid(1, WindowServerId::new(1));
        let w2 = WindowId::with_wsid(1, WindowServerId::new(2));

        let response = reactor.filter_response(
            layout::EventResponse {
                frame_overrides: vec![],
                raise_windows: vec![w2],
                focus_window: Some(w1),
            },
            &[WindowServerId::new(2), WindowServerId::new(1)],
        );

        assert_eq!(response.raise_windows, vec![w2]);
        assert_eq!(response.focus_window, Some(w1));
    }

    #[test]
    fn it_ignores_unmanaged_and_nonzero_layer_windows_when_comparing_space_order() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        let space = SpaceId::new(1);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), None, false));
        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        let desired = reactor
            .layout
            .handle_event(LayoutEvent::SpaceExposed(space, CGSize::new(1000., 1000.)))
            .raise_windows;
        let mut on_screen = vec![
            WindowServerInfo {
                id: WindowServerId::new(90),
                pid: 9,
                layer: 3,
                frame: CGRect::new(CGPoint::new(10., 10.), CGSize::new(100., 100.)),
            },
            WindowServerInfo {
                id: WindowServerId::new(91),
                pid: 9,
                layer: 0,
                frame: CGRect::new(CGPoint::new(2000., 10.), CGSize::new(100., 100.)),
            },
        ];
        on_screen.extend(desired.iter().map(|wid| WindowServerInfo {
            id: reactor.windows[wid].window_server_id.unwrap(),
            pid: wid.pid,
            layer: 0,
            frame: reactor.windows[wid].frame_monotonic,
        }));
        reactor.handle_event(Event::SpaceChanged(
            vec![Some(space)],
            WindowsOnScreen::new(on_screen),
        ));

        assert!(raise_manager_rx.try_recv().is_err());
    }

    #[test]
    fn it_ignores_windows_on_disabled_spaces() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![None],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(1)));

        let state_before = apps.windows.clone();
        let _events = apps.simulate_events();
        assert_eq!(state_before, apps.windows, "Window should not have been moved",);

        // Make sure it doesn't choke on destroyed events for ignored windows.
        reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
        reactor.handle_event(Event::WindowCreated(
            WindowId::new(1, 2),
            make_window(2),
            MouseState::Up,
        ));
        reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
    }

    #[test]
    fn it_keeps_discovered_windows_on_their_initial_screen() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen1, screen2],
            spaces: vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
            scale_factors: vec![2.0, 2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        let mut windows = make_windows(2);
        windows[1].frame.origin = CGPoint::new(1100., 100.);
        reactor.handle_events(apps.make_app(1, windows));

        let _events = apps.simulate_events();
        assert_eq!(
            screen1,
            apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
        );
        assert_eq!(
            screen2,
            apps.windows.get(&WindowId::new(1, 2)).expect("Window was not resized").frame,
        );
    }

    #[test]
    fn it_moves_windows_dragged_between_spaces() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        let space1 = SpaceId::new(1);
        let space2 = SpaceId::new(2);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen1, screen2],
            spaces: vec![Some(space1), Some(space2)],
            scale_factors: vec![2.0, 2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        // Both windows start on screen1 / space1.
        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        reactor.handle_events(apps.simulate_events());

        let dragged = WindowId::new(1, 1);
        let space1_windows: Vec<_> = reactor
            .layout
            .calculate_layout(space1, screen1, &reactor.config)
            .into_iter()
            .map(|(wid, _)| wid)
            .collect();
        assert!(space1_windows.contains(&dragged));
        assert!(reactor.layout.calculate_layout(space2, screen2, &reactor.config).is_empty());

        // Drag window 1 onto screen2, keeping its size the same so this is a
        // pure move (not a resize). The mouse button is still down.
        let frame = apps.windows[&dragged].frame;
        let new_frame = CGRect::new(CGPoint::new(1100., frame.origin.y), frame.size);
        reactor.handle_event(Event::WindowFrameChanged(
            dragged,
            new_frame,
            apps.windows[&dragged].last_seen_txid,
            Requested(false),
            Some(MouseState::Down),
        ));

        // The window now belongs to space2's layout and has left space1.
        let space1_windows: Vec<_> = reactor
            .layout
            .calculate_layout(space1, screen1, &reactor.config)
            .into_iter()
            .map(|(wid, _)| wid)
            .collect();
        let space2_windows: Vec<_> = reactor
            .layout
            .calculate_layout(space2, screen2, &reactor.config)
            .into_iter()
            .map(|(wid, _)| wid)
            .collect();
        assert!(!space1_windows.contains(&dragged), "{space1_windows:?}");
        assert!(space2_windows.contains(&dragged), "{space2_windows:?}");
        assert!(space1_windows.contains(&WindowId::new(1, 2)));

        // The reactor must not write any frames while the drag is in progress.
        assert!(
            apps.requests().is_empty(),
            "reactor shouldn't move windows mid-drag"
        );

        // Releasing the mouse re-tiles the window onto screen2.
        reactor.handle_event(Event::MouseUp);
        let requests = apps.requests();
        assert!(!requests.is_empty(), "release should re-tile the window");
        let events = apps.simulate_events_for_requests(requests);
        for event in events {
            reactor.handle_event(event);
        }
        assert_eq!(screen2, apps.windows[&dragged].frame);
    }

    #[test]
    fn it_keeps_windows_in_space_on_intra_space_drag() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        let space1 = SpaceId::new(1);
        let space2 = SpaceId::new(2);
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen1, screen2],
            spaces: vec![Some(space1), Some(space2)],
            scale_factors: vec![2.0, 2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        reactor.handle_events(apps.simulate_events());

        // Drag a window to a different position but still on screen1 / space1.
        let dragged = WindowId::new(1, 1);
        let frame = apps.windows[&dragged].frame;
        let new_frame = CGRect::new(
            CGPoint::new(frame.origin.x + 20., frame.origin.y + 20.),
            frame.size,
        );
        reactor.handle_event(Event::WindowFrameChanged(
            dragged,
            new_frame,
            apps.windows[&dragged].last_seen_txid,
            Requested(false),
            Some(MouseState::Down),
        ));

        // The window stays on space1 and space2 remains empty.
        let space1_windows: Vec<_> = reactor
            .layout
            .calculate_layout(space1, screen1, &reactor.config)
            .into_iter()
            .map(|(wid, _)| wid)
            .collect();
        assert!(space1_windows.contains(&dragged));
        assert!(reactor.layout.calculate_layout(space2, screen2, &reactor.config).is_empty());

        // No frames are written while the mouse button is still down.
        assert!(
            apps.requests().is_empty(),
            "reactor shouldn't move windows mid-drag"
        );
    }

    /// Neighbors still follow along, and the window is corrected on mouse up.
    #[test]
    fn it_doesnt_write_to_a_window_the_user_is_resizing() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        let resized = WindowId::new(1, 1);
        let neighbor = WindowId::new(1, 2);
        let frame = apps.windows[&resized].frame;

        // The user drags the shared edge left, shrinking window 1.
        let new_frame = CGRect::new(
            frame.origin,
            CGSize::new(frame.size.width - 100., frame.size.height),
        );
        apps.windows.get_mut(&resized).unwrap().frame = new_frame;
        reactor.handle_event(Event::WindowFrameChanged(
            resized,
            new_frame,
            apps.windows[&resized].last_seen_txid,
            Requested(false),
            Some(MouseState::Down),
        ));

        // The neighbor grows to fill the space, but we leave the dragged window
        // alone.
        let wids = written_wids(apps.requests());
        assert!(wids.contains(&neighbor), "neighbor should follow: {wids:?}");
        assert!(
            !wids.contains(&resized),
            "shouldn't write to the window being resized: {wids:?}"
        );

        // The app settles on a change in three directions at once, which the
        // layout tree refuses to apply. The model and the window now disagree.
        let odd_frame = CGRect::new(
            CGPoint::new(new_frame.origin.x + 10., new_frame.origin.y + 10.),
            CGSize::new(new_frame.size.width - 30., new_frame.size.height - 5.),
        );
        apps.windows.get_mut(&resized).unwrap().frame = odd_frame;
        reactor.handle_event(Event::WindowFrameChanged(
            resized,
            odd_frame,
            apps.windows[&resized].last_seen_txid,
            Requested(false),
            Some(MouseState::Down),
        ));
        let wids = written_wids(apps.requests());
        assert!(
            !wids.contains(&resized),
            "shouldn't write to the window being resized: {wids:?}"
        );

        // Releasing the mouse snaps it back to the layout's frame.
        reactor.handle_event(Event::MouseUp);
        let wids = written_wids(apps.requests());
        assert!(
            wids.contains(&resized),
            "release should correct the window: {wids:?}"
        );
    }

    /// The MouseUp event can be lost, e.g. if the event tap is disabled while
    /// the button is down. The mouse state on the next frame change releases
    /// the window instead.
    #[test]
    fn it_stops_suppressing_a_resize_when_the_mouse_up_is_missed() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        let resized = WindowId::new(1, 1);
        let frame = apps.windows[&resized].frame;
        let new_frame = CGRect::new(
            frame.origin,
            CGSize::new(frame.size.width - 100., frame.size.height),
        );
        apps.windows.get_mut(&resized).unwrap().frame = new_frame;
        reactor.handle_event(Event::WindowFrameChanged(
            resized,
            new_frame,
            apps.windows[&resized].last_seen_txid,
            Requested(false),
            Some(MouseState::Down),
        ));
        let wids = written_wids(apps.requests());
        assert!(!wids.contains(&resized), "should be suppressed: {wids:?}");

        // No MouseUp arrives, but the next frame change reports the button up
        // and asks for a change the tree can't apply, so the model and the
        // window disagree.
        let odd_frame = CGRect::new(
            CGPoint::new(new_frame.origin.x + 10., new_frame.origin.y + 10.),
            CGSize::new(new_frame.size.width - 30., new_frame.size.height - 5.),
        );
        apps.windows.get_mut(&resized).unwrap().frame = odd_frame;
        reactor.handle_event(Event::WindowFrameChanged(
            resized,
            odd_frame,
            apps.windows[&resized].last_seen_txid,
            Requested(false),
            Some(MouseState::Up),
        ));
        let wids = written_wids(apps.requests());
        assert!(
            wids.contains(&resized),
            "button up should release the window: {wids:?}"
        );
    }

    fn written_wids(requests: Vec<Request>) -> Vec<WindowId> {
        requests
            .into_iter()
            .flat_map(|request| match request {
                Request::SetWindowFrame(wid, _, _) => vec![wid],
                Request::AnimationFrame { wid, .. } => vec![wid],
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn it_ignores_windows_on_nonzero_layers() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(Event::WindowsOnScreenUpdated {
            pid: None,
            on_screen: WindowsOnScreen::new(vec![WindowServerInfo {
                id: WindowServerId::new(1),
                pid: 1,
                layer: 10,
                frame: CGRect::ZERO,
            }]),
        });

        reactor.handle_events(apps.make_app_without_ws_info(1, make_windows(1), None, true));

        let state_before = apps.windows.clone();
        let _events = apps.simulate_events();
        assert_eq!(state_before, apps.windows, "Window should not have been moved",);

        // Make sure it doesn't choke on destroyed events for ignored windows.
        reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
        reactor.handle_event(Event::WindowCreated(
            WindowId::new(1, 2),
            make_window(2),
            MouseState::Up,
        ));
        reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
    }

    #[test]
    fn handle_layout_response_groups_windows_by_app_and_screen() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;

        let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen1, screen2],
            spaces: vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
            scale_factors: vec![2.0, 2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(2)));

        let mut windows = make_windows(2);
        windows[1].frame.origin = CGPoint::new(1100., 100.);
        reactor.handle_events(apps.make_app(2, windows));

        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}

        reactor.handle_layout_response(layout::EventResponse {
            frame_overrides: vec![],
            raise_windows: vec![
                WindowId::new(1, 1),
                WindowId::new(1, 2),
                WindowId::new(2, 1),
                WindowId::new(2, 2),
            ],
            focus_window: None,
        });
        let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
        match msg {
            raise::Event::RaiseRequest(RaiseRequest {
                raise_windows,
                focus_window,
                app_handles: _,
            }) => {
                let raise_windows: HashSet<Vec<WindowId>> = raise_windows.into_iter().collect();
                let expected = [
                    vec![WindowId::new(1, 1), WindowId::new(1, 2)],
                    vec![WindowId::new(2, 1)],
                    vec![WindowId::new(2, 2)],
                ]
                .into_iter()
                .collect();
                assert_eq!(raise_windows, expected);
                assert!(focus_window.is_none());
            }
            _ => panic!("Unexpected event: {msg:?}"),
        }
    }

    #[test]
    fn handle_layout_response_includes_handles_for_raise_and_focus_windows() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;

        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_events(apps.make_app(2, make_windows(1)));

        let _events = apps.simulate_events();
        while raise_manager_rx.try_recv().is_ok() {}
        reactor.handle_layout_response(layout::EventResponse {
            frame_overrides: vec![],
            raise_windows: vec![WindowId::new(1, 1)],
            focus_window: Some(WindowId::new(2, 1)),
        });
        let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
        match msg {
            raise::Event::RaiseRequest(RaiseRequest { app_handles, .. }) => {
                assert!(app_handles.contains_key(&1));
                assert!(app_handles.contains_key(&2));
            }
            _ => panic!("Unexpected event: {msg:?}"),
        }
    }

    #[test]
    fn it_preserves_layout_after_login_screen() {
        // TODO: This would be better tested with a more complete simulation.
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(3),
            Some(WindowId::new(1, 1)),
            true,
        ));
        reactor.handle_event(Event::StartupComplete);
        reactor.handle_event(Event::ApplicationGloballyActivated(1));
        apps.simulate_until_quiet(&mut reactor);
        let default = reactor.layout.calculate_layout(space, full_screen, &reactor.config);

        assert!(reactor.layout.selected_window(space).is_some());
        reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
            Direction::Up,
        ))));
        apps.simulate_until_quiet(&mut reactor);
        let modified = reactor.layout.calculate_layout(space, full_screen, &reactor.config);
        assert_ne!(default, modified);

        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![CGRect::ZERO],
            spaces: vec![None],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(Event::WindowsOnScreenUpdated {
            pid: None,
            on_screen: WindowsOnScreen::new(
                (1..=3)
                    .map(|n| WindowServerInfo {
                        pid: 1,
                        id: WindowServerId::new(n),
                        layer: 0,
                        frame: CGRect::ZERO,
                    })
                    .collect(),
            ),
        });
        let requests = apps.requests();
        for request in requests {
            match request {
                Request::GetVisibleWindows => {
                    // Simulate the login screen condition: No windows are
                    // considered visible by the accessibility API, but they are
                    // from the window server API in the event above.
                    reactor.handle_event(Event::WindowsDiscovered {
                        pid: 1,
                        new: vec![],
                        known_visible: vec![],
                    });
                }
                req => {
                    let events = apps.simulate_events_for_requests(vec![req]);
                    for event in events {
                        reactor.handle_event(event);
                    }
                }
            }
        }
        apps.simulate_until_quiet(&mut reactor);

        assert_eq!(
            reactor.layout.calculate_layout(space, full_screen, &reactor.config),
            modified
        );
    }

    #[test]
    fn it_fixes_window_sizes_after_screen_config_changes() {
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(SpaceId::new(1))],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        reactor.handle_events(apps.make_app(1, make_windows(1)));
        reactor.handle_event(Event::StartupComplete);

        let _events = apps.simulate_events();
        assert_eq!(
            full_screen,
            apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
        );

        // Simulate the system resizing a window after it recognizes an old
        // configurations. Resize events are not sent in this case.
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![
                full_screen,
                CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.)),
            ],
            spaces: vec![Some(SpaceId::new(1)), None],
            scale_factors: vec![2.0, 2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(Event::WindowsOnScreenUpdated {
            pid: None,
            on_screen: WindowsOnScreen::new(vec![WindowServerInfo {
                id: WindowServerId::new(1),
                pid: 1,
                layer: 0,
                frame: CGRect::new(CGPoint::new(500., 0.), CGSize::new(500., 500.)),
            }]),
        });

        let _events = apps.simulate_events();
        assert_eq!(
            full_screen,
            apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
        );
    }

    #[test]
    fn it_doesnt_crash_after_main_window_closes() {
        use Direction::*;
        use Event::*;
        use LayoutCommand::*;

        use super::Command::*;
        use super::Reactor;
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        reactor.handle_event(ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        assert_eq!(None, reactor.main_window());

        reactor.handle_event(ApplicationGloballyActivated(1));
        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
        ));

        reactor.handle_event(WindowDestroyed(WindowId::new(1, 1)));
        reactor.handle_event(Command(Layout(MoveFocus(Left))));
    }

    #[test]
    fn move_focus_uses_active_screen_when_no_window_is_focused() {
        use Direction::*;
        use Event::*;
        use LayoutCommand::*;

        use super::Command::*;
        use super::Reactor;

        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        reactor.handle_event(ScreenParametersChanged {
            frames: vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(ApplicationGloballyActivated(1));
        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
        ));
        assert_eq!(reactor.main_window(), Some(WindowId::new(1, 1)));

        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;
        reactor.handle_event(ApplicationGloballyDeactivated(1));
        assert_eq!(reactor.main_window(), None);

        reactor.handle_event(Command(Layout(MoveFocus(Right))));

        let event = raise_manager_rx
            .try_recv()
            .expect("focus command should produce a raise request")
            .1;
        let raise::Event::RaiseRequest(RaiseRequest { focus_window, .. }) = event else {
            panic!("unexpected raise event: {event:?}");
        };
        assert_eq!(focus_window.map(|(wid, _)| wid), Some(WindowId::new(1, 2)));
    }

    #[test]
    fn it_follows_the_mouse_only_when_the_main_window_has_keyboard_focus() {
        use Event::*;

        let mut apps = Apps::new();
        let space = SpaceId::new(1);
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor.handle_event(ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor.handle_event(ApplicationGloballyActivated(1));
        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
        ));
        reactor.handle_events(apps.simulate_events());
        assert_eq!(reactor.main_window(), Some(WindowId::new(1, 1)));

        let (raise_manager_tx, mut raise_manager_rx) = mpsc::unbounded_channel();
        reactor.raise_manager_tx = raise_manager_tx;

        // Another process has keyboard focus, as when Spotlight is open.
        reactor.handle_event(MouseMovedOverWindow(WindowServerId::new(2), Some(2)));
        assert!(
            raise_manager_rx.try_recv().is_err(),
            "mouse move should be ignored while another process has keyboard focus"
        );

        // The main window's app has keyboard focus.
        reactor.handle_event(MouseMovedOverWindow(WindowServerId::new(2), Some(1)));
        let event = raise_manager_rx
            .try_recv()
            .expect("mouse move should produce a raise request")
            .1;
        let raise::Event::RaiseRequest(RaiseRequest { focus_window, .. }) = event else {
            panic!("unexpected raise event: {event:?}");
        };
        assert_eq!(focus_window.map(|(wid, _)| wid), Some(WindowId::new(1, 2)));

        // The key focus process could not be read.
        reactor.handle_event(MouseMovedOverWindow(WindowServerId::new(1), None));
        assert!(
            raise_manager_rx.try_recv().is_ok(),
            "mouse move should be followed when the key focus process is unknown"
        );
    }

    #[test]
    fn it_removes_terminated_app_windows_on_startup_complete() {
        use Event::*;

        let mut apps = Apps::new();
        let space = SpaceId::new(1);
        let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

        // First reactor: simulate the state before shutdown with three apps running
        let mut reactor1 = Reactor::new_for_test(LayoutManager::new_for_test());
        reactor1.handle_event(ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        reactor1.handle_events(apps.make_app(1, make_windows(2)));
        reactor1.handle_events(apps.make_app(2, make_windows(2)));
        reactor1.handle_events(apps.make_app(3, make_windows(1)));
        apps.simulate_until_quiet(&mut reactor1);

        // Verify all 5 windows are in the layout
        let layout_before = reactor1.layout.calculate_layout(space, full_screen, &reactor1.config);
        assert_eq!(layout_before.len(), 5, "Expected 5 windows before shutdown");

        // Serialize the layout to simulate saving state before shutdown
        let serialized_layout = ron::ser::to_string(&reactor1.layout).unwrap();

        // Second reactor: simulate restore after reboot, where app 2 was terminated
        // and doesn't launch again
        let restored_layout: LayoutManager = ron::de::from_str(&serialized_layout).unwrap();
        let mut apps2 = Apps::new();
        let mut reactor2 = Reactor::new_for_test(restored_layout);
        reactor2.handle_event(ScreenParametersChanged {
            frames: vec![full_screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });
        // Only apps 1 and 3 launch during restore (app 2 was terminated between save and restore)
        reactor2.handle_events(apps2.make_app(1, make_windows(2)));
        reactor2.handle_events(apps2.make_app(3, make_windows(1)));
        apps2.simulate_until_quiet(&mut reactor2);

        // Before StartupComplete, the layout still contains ghost nodes for app 2's windows
        let layout_before_cleanup =
            reactor2.layout.calculate_layout(space, full_screen, &reactor2.config);
        assert_eq!(layout_before_cleanup.len(), 5);

        // Send StartupComplete to trigger cleanup of terminated app windows
        reactor2.handle_event(StartupComplete);

        // After StartupComplete, verify that windows from terminated app 2 are removed
        // but windows from running apps 1 and 3 remain
        let windows_after = reactor2
            .layout
            .calculate_layout(space, full_screen, &reactor2.config)
            .into_iter()
            .map(|(wid, _)| wid)
            .sorted()
            .collect_vec();

        assert_eq!(
            windows_after,
            &[
                WindowId::new(1, 1),
                WindowId::new(1, 2),
                WindowId::new(3, 1),
            ]
        );
    }

    #[test]
    fn no_scroll_animation_when_idle() {
        let mut reactor = Reactor::new_for_test(LayoutManager::new_for_test());
        let space = SpaceId::new(1);
        let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        reactor.handle_event(Event::ScreenParametersChanged {
            frames: vec![screen],
            spaces: vec![Some(space)],
            scale_factors: vec![2.0],
            converter: CoordinateConverter::default(),
            on_screen: Default::default(),
        });

        let mut apps = Apps::new();
        reactor.handle_events(apps.make_app(1, make_windows(2)));
        reactor.handle_event(Event::StartupComplete);
        apps.simulate_until_quiet(&mut reactor);

        assert!(
            !reactor.layout.has_active_scroll_animation(),
            "timer should be dormant when no scroll animation is active"
        );
    }
}
