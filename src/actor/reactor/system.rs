// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Actions of the Reactor that change the system outside the app threads.
//!
//! Only [`Reactor::spawn`](super::Reactor::spawn) installs [`LiveSystem`], so
//! tests and replays never launch apps or start timers. Everything that affects
//! later decisions comes back to the Reactor as a recorded [`Event`].

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tracing::info;

use super::{Event, Sender, SpaceMoveId};
use crate::actor::app::WindowId;
use crate::actor::{self, window_server};
use crate::model::scratchpad::PendingShowId;
use crate::sys::app::{self, LaunchError, pid_t};
use crate::sys::screen::SpaceId;
use crate::sys::space_move::{self, SpaceMoveError};
use crate::sys::timer::Timer;
use crate::sys::window_server::WindowServerId;

/// How long a scratchpad window may take to appear after its app is launched
/// before it is no longer shown automatically.
pub const SCRATCHPAD_SHOW_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a moved window may take to appear on screen before it is shown
/// wherever it is.
pub const SPACE_MOVE_TIMEOUT: Duration = Duration::from_secs(1);

pub trait SystemActions {
    /// Starts the app of the scratchpad `name`, whose show is pending as
    /// `show`. [`Event::ScratchpadShowExpired`] follows when the launch fails
    /// or the timeout passes.
    fn launch_app(
        &mut self,
        name: &str,
        bundle_id: &str,
        show: PendingShowId,
    ) -> Result<(), LaunchError>;

    /// Starts moving a window to a space. Its visible windows are checked
    /// again afterwards, and [`Event::ScratchpadMoveEnded`] follows after a
    /// timeout. On error nothing was started.
    fn move_window_to_space(
        &mut self,
        wid: WindowId,
        wsid: WindowServerId,
        space: SpaceId,
        id: SpaceMoveId,
    ) -> Result<(), SpaceMoveError>;
}

/// Performs no actions. Checks the bundle id like [`LiveSystem`].
pub struct NoSystem;

impl SystemActions for NoSystem {
    fn launch_app(
        &mut self,
        _name: &str,
        bundle_id: &str,
        _show: PendingShowId,
    ) -> Result<(), LaunchError> {
        app::validate_bundle_id(bundle_id)?;
        info!(bundle_id, "Not launching app without a live system");
        Ok(())
    }

    fn move_window_to_space(
        &mut self,
        wid: WindowId,
        _wsid: WindowServerId,
        space: SpaceId,
        _id: SpaceMoveId,
    ) -> Result<(), SpaceMoveError> {
        info!(?wid, ?space, "Not moving window without a live system");
        Ok(())
    }
}

/// Starts `open` for a bundle id and calls back with whether it succeeded.
type LaunchFn = fn(&str, Box<dyn FnOnce(bool) + Send>) -> Result<(), LaunchError>;

type MoveFn = fn(WindowServerId, SpaceId) -> Result<(), SpaceMoveError>;

/// The pid that owns a window, according to the window server.
type OwnerFn = fn(WindowServerId) -> Option<pid_t>;

pub struct LiveSystem {
    reactor_tx: Sender,
    ws_tx: window_server::Sender,
    delayed_tx: actor::Sender<DelayedEvent>,
    launch: LaunchFn,
    move_window: MoveFn,
    window_owner: OwnerFn,
}

impl LiveSystem {
    pub fn new(
        reactor_tx: Sender,
        ws_tx: window_server::Sender,
        delayed_tx: actor::Sender<DelayedEvent>,
    ) -> LiveSystem {
        LiveSystem {
            reactor_tx,
            ws_tx,
            delayed_tx,
            launch: |bundle_id, on_exit| app::launch_app_then(bundle_id, on_exit),
            move_window: space_move::move_window_to_space,
            window_owner: |wsid| crate::sys::window_server::get_window(wsid).map(|info| info.pid),
        }
    }
}

impl SystemActions for LiveSystem {
    fn launch_app(
        &mut self,
        name: &str,
        bundle_id: &str,
        show: PendingShowId,
    ) -> Result<(), LaunchError> {
        let reactor_tx = self.reactor_tx.clone();
        (self.launch)(
            bundle_id,
            Box::new(move |launched| {
                if !launched {
                    reactor_tx.send(Event::ScratchpadShowExpired(show));
                }
            }),
        )?;
        self.delayed_tx.send(DelayedEvent {
            key: DelayKey::ScratchpadShow(name.to_owned()),
            delay: SCRATCHPAD_SHOW_TIMEOUT,
            event: Event::ScratchpadShowExpired(show),
        });
        Ok(())
    }

    fn move_window_to_space(
        &mut self,
        wid: WindowId,
        wsid: WindowServerId,
        space: SpaceId,
        id: SpaceMoveId,
    ) -> Result<(), SpaceMoveError> {
        // The window server id comes from the app, so it may name a window of
        // another app.
        if (self.window_owner)(wsid) != Some(wid.pid) {
            return Err(SpaceMoveError::NotOwned);
        }
        (self.move_window)(wsid, space)?;
        self.ws_tx.send(window_server::Event::RecheckVisibleWindows(wid.pid));
        self.delayed_tx.send(DelayedEvent {
            key: DelayKey::SpaceMove(wid),
            delay: SPACE_MOVE_TIMEOUT,
            event: Event::ScratchpadMoveEnded(id),
        });
        Ok(())
    }
}

/// An event to send to the Reactor after a delay.
#[derive(Debug)]
pub struct DelayedEvent {
    /// A later event with the same key replaces this one.
    key: DelayKey,
    delay: Duration,
    event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DelayKey {
    ScratchpadShow(String),
    SpaceMove(WindowId),
}

/// Pending delayed events, at most one per key.
#[derive(Default)]
struct Deadlines(BTreeMap<DelayKey, (Instant, Event)>);

impl Deadlines {
    fn insert(&mut self, now: Instant, delayed: DelayedEvent) {
        self.0.insert(delayed.key, (now + delayed.delay, delayed.event));
    }

    fn next(&self) -> Option<Instant> {
        self.0.values().map(|(at, _)| *at).min()
    }

    fn take_due(&mut self, now: Instant) -> Vec<Event> {
        let due: Vec<DelayKey> = self
            .0
            .iter()
            .filter(|(_, (at, _))| *at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        due.into_iter()
            .filter_map(|key| self.0.remove(&key))
            .map(|(_, event)| event)
            .collect()
    }
}

/// Sends each [`DelayedEvent`] to the Reactor once its delay has passed.
pub async fn run_delayed_events(mut rx: actor::Receiver<DelayedEvent>, reactor_tx: Sender) {
    let mut deadlines = Deadlines::default();
    let mut timer = Timer::manual();
    loop {
        if let Some(at) = deadlines.next() {
            timer.set_next_fire(at.saturating_duration_since(Instant::now()));
        }
        tokio::select! {
            delayed = rx.recv() => {
                let Some((_span, delayed)) = delayed else { break };
                deadlines.insert(Instant::now(), delayed);
            }
            _ = timer.next(), if deadlines.next().is_some() => {
                for event in deadlines.take_due(Instant::now()) {
                    reactor_tx.send(event);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::actor::reactor;

    fn show_id(event: &Event) -> PendingShowId {
        match event {
            Event::ScratchpadShowExpired(id) => *id,
            _ => panic!("unexpected event {event:?}"),
        }
    }

    fn ids(events: &[Event]) -> Vec<PendingShowId> {
        events.iter().map(show_id).collect()
    }

    /// Pending show ids for tests, which can't construct them directly.
    fn pending_ids(count: usize) -> Vec<PendingShowId> {
        let mut scratchpads = crate::model::scratchpad::Scratchpads::default();
        (0..count)
            .map(|_| {
                _ = scratchpads.toggle("k", Some("com.example"), |_| unreachable!());
                scratchpads.pending_show("k").unwrap()
            })
            .collect()
    }

    fn delayed(name: &str, delay_ms: u64, id: PendingShowId) -> DelayedEvent {
        DelayedEvent {
            key: DelayKey::ScratchpadShow(name.into()),
            delay: Duration::from_millis(delay_ms),
            event: Event::ScratchpadShowExpired(id),
        }
    }

    #[test]
    fn deadlines_keep_the_last_event_per_key() {
        let ids = pending_ids(101);
        let start = Instant::now();
        let mut deadlines = Deadlines::default();
        for (i, &id) in ids[..100].iter().enumerate() {
            deadlines.insert(start + Duration::from_millis(i as u64), delayed("k", 10, id));
        }
        deadlines.insert(start, delayed("other", 50, ids[100]));
        assert_eq!(deadlines.0.len(), 2);
        assert_eq!(deadlines.next(), Some(start + Duration::from_millis(50)));

        assert!(deadlines.take_due(start + Duration::from_millis(49)).is_empty());
        assert_eq!(
            super::tests::ids(&deadlines.take_due(start + Duration::from_millis(109))),
            [ids[99], ids[100]]
        );
        assert_eq!(deadlines.next(), None);
        assert!(deadlines.take_due(start + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_later_event_for_a_key_replaces_an_earlier_deadline_either_way() {
        let ids = pending_ids(3);
        let start = Instant::now();
        let mut deadlines = Deadlines::default();
        deadlines.insert(start, delayed("k", 100, ids[0]));
        // A shorter delay for the same key moves the deadline earlier.
        deadlines.insert(start, delayed("k", 10, ids[1]));
        assert_eq!(deadlines.next(), Some(start + Duration::from_millis(10)));
        // And a longer one moves it later.
        deadlines.insert(start, delayed("k", 200, ids[2]));
        assert_eq!(deadlines.next(), Some(start + Duration::from_millis(200)));
        assert!(deadlines.take_due(start + Duration::from_millis(199)).is_empty());
        assert_eq!(
            super::tests::ids(&deadlines.take_due(start + Duration::from_millis(200))),
            [ids[2]]
        );
    }

    #[test]
    fn show_and_move_keys_do_not_replace_each_other() {
        let [id] = pending_ids(1)[..] else { unreachable!() };
        let start = Instant::now();
        let mut deadlines = Deadlines::default();
        let wid = WindowId::new(2, 1);
        let moved = |move_id| DelayedEvent {
            key: DelayKey::SpaceMove(wid),
            delay: Duration::from_millis(5),
            event: Event::ScratchpadMoveEnded(SpaceMoveId(move_id)),
        };
        deadlines.insert(start, moved(1));
        deadlines.insert(start, delayed("k", 5, id));
        deadlines.insert(start, moved(2));
        deadlines.insert(
            start,
            DelayedEvent {
                key: DelayKey::SpaceMove(WindowId::new(2, 2)),
                delay: Duration::from_millis(20),
                event: Event::ScratchpadMoveEnded(SpaceMoveId(3)),
            },
        );
        let due = deadlines.take_due(start + Duration::from_millis(5));
        assert_eq!(due.len(), 2, "{due:?}");
        assert!(due.iter().any(|e| matches!(e, Event::ScratchpadShowExpired(s) if *s == id)));
        assert!(due.iter().any(|e| matches!(e, Event::ScratchpadMoveEnded(SpaceMoveId(2)))));
        assert_eq!(deadlines.next(), Some(start + Duration::from_millis(20)));
        let due = deadlines.take_due(start + Duration::from_millis(20));
        assert!(matches!(due[..], [Event::ScratchpadMoveEnded(SpaceMoveId(3))]));
    }

    #[test]
    fn delayed_events_are_sent_after_their_delay_once_per_key() {
        use crate::sys::executor::Executor;
        use crate::sys::timer::Timer;

        let ids = pending_ids(3);
        let (reactor_tx, mut reactor_rx) = reactor::channel();
        let (delayed_tx, delayed_rx) = actor::channel();
        let started = Instant::now();
        let mut received = vec![];
        Executor::run(async {
            let run = run_delayed_events(delayed_rx, reactor_tx);
            let drive = async {
                delayed_tx.send(delayed("k", 150, ids[0]));
                delayed_tx.send(delayed("k", 60, ids[1]));
                delayed_tx.send(delayed("l", 30, ids[2]));
                Timer::sleep(Duration::from_millis(15)).await;
                assert!(reactor_rx.try_recv().is_err(), "sent before the delay");
                let deadline = started + Duration::from_secs(5);
                while received.len() < 2 && Instant::now() < deadline {
                    while let Ok((_, event)) = reactor_rx.try_recv() {
                        received.push((show_id(&event), started.elapsed()));
                    }
                    Timer::sleep(Duration::from_millis(5)).await;
                }
                // Past the replaced deadline of k.
                Timer::sleep(Duration::from_millis(200)).await;
                while let Ok((_, event)) = reactor_rx.try_recv() {
                    received.push((show_id(&event), started.elapsed()));
                }
                drop(delayed_tx);
            };
            tokio::join!(run, drive);
        });
        let order: Vec<_> = received.iter().map(|(id, _)| *id).collect();
        assert_eq!(order, [ids[2], ids[1]], "the replaced event for k is not sent");
        assert!(received[0].1 >= Duration::from_millis(30), "{received:?}");
        assert!(received[1].1 >= Duration::from_millis(60), "{received:?}");
    }

    thread_local! {
        static LAUNCH_CALLBACKS: RefCell<Vec<Box<dyn FnOnce(bool) + Send>>> =
            const { RefCell::new(Vec::new()) };
    }

    /// Starts nothing and keeps the callback, as if `open` never returned.
    fn hanging_launch(
        bundle_id: &str,
        on_exit: Box<dyn FnOnce(bool) + Send>,
    ) -> Result<(), LaunchError> {
        app::validate_bundle_id(bundle_id)?;
        LAUNCH_CALLBACKS.with_borrow_mut(|callbacks| callbacks.push(on_exit));
        Ok(())
    }

    fn live_system() -> (LiveSystem, reactor::Receiver, actor::Receiver<DelayedEvent>) {
        let (reactor_tx, reactor_rx) = reactor::channel();
        let (ws_tx, _ws_rx) = actor::channel();
        let (delayed_tx, delayed_rx) = actor::channel();
        let mut system = LiveSystem::new(reactor_tx, ws_tx, delayed_tx);
        system.launch = hanging_launch;
        system.move_window = |_, _| panic!("tests must not move windows");
        system.window_owner = |_| panic!("tests must not query the window server");
        (system, reactor_rx, delayed_rx)
    }

    #[test]
    fn move_checks_the_windows_again_and_ends_after_a_timeout() {
        let (reactor_tx, _reactor_rx) = reactor::channel();
        let (ws_tx, mut ws_rx) = actor::channel();
        let (delayed_tx, mut delayed_rx) = actor::channel();
        let mut system = LiveSystem::new(reactor_tx, ws_tx, delayed_tx);
        system.move_window = |wsid, space| {
            assert_eq!((wsid, space), (WindowServerId::new(9), SpaceId::new(4)));
            Ok(())
        };
        system.window_owner = |wsid| (wsid == WindowServerId::new(9)).then_some(3);
        let wid = WindowId::new(3, 1);
        let id = SpaceMoveId(7);
        system
            .move_window_to_space(wid, WindowServerId::new(9), SpaceId::new(4), id)
            .unwrap();
        let (_, recheck) = ws_rx.try_recv().unwrap();
        assert!(matches!(recheck, window_server::Event::RecheckVisibleWindows(3)));
        let (_, delayed) = delayed_rx.try_recv().unwrap();
        assert_eq!(delayed.key, DelayKey::SpaceMove(wid));
        assert_eq!(delayed.delay, SPACE_MOVE_TIMEOUT);
        assert!(matches!(delayed.event, Event::ScratchpadMoveEnded(ended) if ended == id));
    }

    #[test]
    fn unsupported_move_schedules_nothing() {
        let (reactor_tx, _reactor_rx) = reactor::channel();
        let (ws_tx, mut ws_rx) = actor::channel();
        let (delayed_tx, mut delayed_rx) = actor::channel();
        let mut system = LiveSystem::new(reactor_tx, ws_tx, delayed_tx);
        system.move_window = |_, _| Err(SpaceMoveError::Unsupported);
        system.window_owner = |_| Some(3);
        let result = system.move_window_to_space(
            WindowId::new(3, 1),
            WindowServerId::new(9),
            SpaceId::new(4),
            SpaceMoveId(0),
        );
        assert!(matches!(result, Err(SpaceMoveError::Unsupported)));
        assert!(ws_rx.try_recv().is_err());
        assert!(delayed_rx.try_recv().is_err());
    }

    #[test]
    fn window_of_another_app_is_not_moved() {
        let (reactor_tx, _reactor_rx) = reactor::channel();
        let (ws_tx, mut ws_rx) = actor::channel();
        let (delayed_tx, mut delayed_rx) = actor::channel();
        let mut system = LiveSystem::new(reactor_tx, ws_tx, delayed_tx);
        system.move_window = |_, _| panic!("the window of another app must not be moved");
        for owner in [|_| Some(4), |_| None] {
            system.window_owner = owner;
            let result = system.move_window_to_space(
                WindowId::new(3, 1),
                WindowServerId::new(9),
                SpaceId::new(4),
                SpaceMoveId(0),
            );
            assert!(matches!(result, Err(SpaceMoveError::NotOwned)));
        }
        assert!(ws_rx.try_recv().is_err());
        assert!(delayed_rx.try_recv().is_err());
    }

    #[test]
    fn launch_expires_after_the_timeout_even_if_open_hangs() {
        let [id] = pending_ids(1)[..] else { unreachable!() };
        let (mut system, mut reactor_rx, mut delayed_rx) = live_system();
        system.launch_app("k", "com.example.hangs", id).unwrap();
        let (_, delayed) = delayed_rx.try_recv().unwrap();
        assert_eq!(delayed.key, DelayKey::ScratchpadShow("k".into()));
        assert_eq!(delayed.delay, SCRATCHPAD_SHOW_TIMEOUT);
        assert_eq!(show_id(&delayed.event), id);
        assert!(reactor_rx.try_recv().is_err());
    }

    #[test]
    fn failed_open_expires_the_show_at_once() {
        let ids = pending_ids(2);
        let (mut system, mut reactor_rx, _delayed_rx) = live_system();
        system.launch_app("k", "com.example.fails", ids[0]).unwrap();
        system.launch_app("l", "com.example.works", ids[1]).unwrap();
        let [fails, works] = LAUNCH_CALLBACKS.take().try_into().ok().unwrap();
        works(true);
        fails(false);
        let (_, event) = reactor_rx.try_recv().unwrap();
        assert_eq!(show_id(&event), ids[0]);
        assert!(reactor_rx.try_recv().is_err());
    }

    #[test]
    fn invalid_bundle_id_neither_launches_nor_schedules() {
        let [id] = pending_ids(1)[..] else { unreachable!() };
        let (mut system, _reactor_rx, mut delayed_rx) = live_system();
        assert!(system.launch_app("k", "-a", id).is_err());
        assert!(delayed_rx.try_recv().is_err());
        assert!(NoSystem.launch_app("k", "-a", id).is_err());
        assert_eq!(NoSystem.launch_app("k", "com.example", id), Ok(()));
    }
}
