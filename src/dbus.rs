//! The D-Bus bridge: both frozen interfaces, served headlessly.
//!
//! Nothing here decides what a notification *means* (that is `store.rs`'s
//! pure model) or shows anything on screen (that is `main.rs`'s layershell
//! daemon and `modules/toast.rs`) — this file's whole job is to answer the
//! bus correctly and hand raw, unparsed call data across a channel for
//! `main.rs` to interpret.
//!
//! **Stage 5 changed who drives this file.** Stage 3 shipped a `run`
//! function that connected, called [`serve`], parked on
//! `std::future::pending()` to keep the connection alive, and called
//! `std::process::exit(0)` for the second-instance case. All of that now
//! lives in `main.rs`'s `dbus_worker_stream` — an `iced::Subscription`,
//! because a `process::exit` from inside a stream tears the process down
//! mid-frame instead of letting iced's own event loop shut down cleanly.
//! [`serve`] is this module's only entry point; [`emit_notification_closed`],
//! [`emit_action_invoked`] (Stage 6) and [`sync_control_state`] (Stage 9) are
//! its only exits.
//!
//! # Serving vs. proxying (teaching note, same split as
//! `saola-capture::dbus` and `saola-session::modules::inhibit`)
//!
//! Two independent `#[zbus::interface]` blocks live in this file, one per
//! frozen contract (PLAN.md's "Frozen external contracts" section, and the
//! saola-files two-interface rule AGENTS.md names): [`NotificationsService`]
//! serves `org.freedesktop.Notifications` — the interface every notifying
//! app (`notify-send`, browsers, chat clients) already knows how to call,
//! regardless of which desktop it's running on. [`ControlService`] serves
//! `io.saola.Notifications1` — this desktop's own contract, the one the
//! saola-panel indicator (and later this crate's own toast/centre modules)
//! will drive. Both are plain Rust structs whose inherent `impl` blocks are
//! rewritten by the `#[zbus::interface(name = "...")]` macro into a real
//! `zbus::object_server::Interface` implementation; nothing in this crate
//! calls their methods directly — `zbus::ObjectServer` dispatches incoming
//! bus calls onto them once [`serve`] registers an instance of each at its
//! object path.
//!
//! # Why every served method forwards a [`DaemonEvent`] instead of *doing*
//! anything
//!
//! There is no store, no toast stack, and no centre yet — Stages 4/5 build
//! those. So every method below does the minimum a bus caller is owed
//! (validate nothing yet, log the call, hand back the one value the spec
//! promises) and then offers the raw call data to whoever eventually
//! listens on the other end of `events`. `try_send`, never
//! `.send().await`, at every one of those hand-offs — Architecture's rule,
//! restated here because it is the one thing in this file most likely to
//! be copied wrong later: blocking a D-Bus method reply on some other
//! task's event loop keeping up would turn a slow UI frame into a hung
//! `notify-send`. A full channel (or a receiver that's gone) degrades to a
//! logged warning; the bus caller still gets its answer either way.
//!
//! # Name claims (teaching note — read before touching [`serve`])
//!
//! Both well-known names are requested with `RequestNameFlags::DoNotQueue`
//! **alone** — never `ReplaceExisting`, per
//! `saola-session::modules::inhibit`'s own load-bearing warning about that
//! flag (its module doc comment explains exactly what goes wrong if a
//! claim ever sets it: silently breaking whichever service already owned
//! the name for every other app on the machine). Object registration
//! always happens *before* the matching name request (same "object first,
//! name second" rule both `saola-capture::dbus::serve` and
//! `saola-session::modules::inhibit::run` document) so there is never a
//! window where a caller sees a name appear on the bus and finds nothing
//! answering at its path.
//!
//! The two names carry **different consequences** when already taken —
//! this is the one place this file's behavior genuinely branches:
//!
//! - `org.freedesktop.Notifications` taken means mako, dunst, or some other
//!   notification daemon already owns it. That is a completely normal
//!   desktop state (Jordan may not have removed the old daemon yet), so
//!   this degrades to "stay inert on that interface, log it, keep running"
//!   — the control interface (and everything built on it later) still
//!   works.
//! - `io.saola.Notifications1` taken means another **`saola-notifications`
//!   process** already owns it — nothing else could legitimately claim
//!   this desktop's own reverse-DNS name. That is a genuine second
//!   instance. [`serve`] reports that as
//!   [`ServeOutcome::AlreadySecondInstance`] and `main.rs` shuts the daemon
//!   down with `iced::exit()` (process exit code 0) rather than let two
//!   daemons fight over one D-Bus contract.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use iced::futures::channel::mpsc;
use zbus::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Structure, Value};

use crate::store;

/// The well-known bus name and object path for the freedesktop
/// notification-daemon contract every notifying app already speaks.
pub const NOTIFICATIONS_SERVICE_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// The well-known bus name and object path for this desktop's own control
/// contract (PLAN.md Frozen external contracts) — the saola-panel
/// indicator's seam into this daemon.
pub const CONTROL_SERVICE_NAME: &str = "io.saola.Notifications1";
pub const CONTROL_OBJECT_PATH: &str = "/io/saola/Notifications1";

// ============================================================================
// IdAllocator — the one piece of this file's behavior pure enough, and
// specific enough, to be worth a unit test rather than only manual
// `busctl` evidence. No zbus types, no async, no `Arc`.
// ============================================================================

/// Allocates notification ids for `Notify`, per PLAN.md Architecture:
/// "`AtomicU32`, start 1, skip 0 on wrap". `0` is reserved by the
/// freedesktop spec to mean "no notification" (it is never a valid id to
/// hand back), so this must never return it — including the one time in
/// four billion calls where a plain wrapping counter would.
///
/// `AtomicU32` rather than a `Mutex<u32>`: the interior mutability an
/// `&self`-only method needs (every `#[zbus::interface]` method takes
/// `&self`, never `&mut self` — zbus dispatches concurrent calls) with no
/// lock to poison and nothing to hold across an `.await`.
#[derive(Debug)]
struct IdAllocator {
    next: AtomicU32,
}

impl IdAllocator {
    fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
        }
    }

    /// Test-only: start the counter somewhere other than `1`, so
    /// wraparound can be exercised directly rather than by actually
    /// allocating four billion ids.
    #[cfg(test)]
    fn starting_at(next: u32) -> Self {
        Self {
            next: AtomicU32::new(next),
        }
    }

    /// Allocates and returns the next id, wrapping past `u32::MAX` and
    /// skipping the reserved `0`.
    ///
    /// `AtomicU32::fetch_add` itself already wraps `u32::MAX -> 0` per its
    /// own documented semantics — no overflow check needed, and this
    /// crate's no-panic rule needs none here. The loop exists purely to
    /// skip the one value the spec reserves; it runs at most twice per
    /// call, since `fetch_add(1)` can never hand back the same skipped
    /// value on two consecutive iterations.
    fn allocate(&self) -> u32 {
        loop {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }
}

/// The frozen `GetCapabilities` reply (PLAN.md Frozen external contracts):
/// v0.1 supports `body`, `actions`, `icon-static`, and `persistence` —
/// deliberately not `body-markup` (Stage 4 strips markup at parse time
/// rather than rendering it), `sound`, or `action-icons`.
fn capabilities() -> Vec<&'static str> {
    vec!["body", "actions", "icon-static", "persistence"]
}

// ============================================================================
// DaemonEvent — the in-process bridge from a served bus call to whatever
// eventually consumes it. This is not part of either wire contract; it is
// how this file hands raw call data across the `mpsc` channel to Stage 5's
// iced `Daemon::update` (via `main.rs`'s own drain loop, for now).
// ============================================================================

/// One notification-shaped or control-shaped thing a bus caller asked for.
///
/// Every variant carries exactly the arguments its served method received
/// — nothing is interpreted here (Stage 4's `store.rs` owns hint parsing,
/// urgency, image decode, and markup stripping; this stage only logs and
/// forwards). `#[derive(Debug)]` is what lets `main.rs`'s drain loop log
/// each event with `tracing::info!(?event, ..)` without every future stage
/// having to write its own `Display`.
///
/// Stages 3 and 4 carried a `#[allow(dead_code)]` here, because every field
/// was *constructed* by a served method and never *read* (the Stage 3 drain
/// loop only formatted whole events via `Debug`, which dead-code analysis
/// does not count as a read). **Stage 5 removed it**:
/// `main.rs::dbus_worker_stream` now matches on every variant and reads
/// every field on its way into the daemon's own `Message`.
#[derive(Debug)]
pub enum DaemonEvent {
    /// A `Notify` call. `id` is already resolved (a fresh allocation, or
    /// `replaces_id` echoed back) by the time this is sent — see
    /// [`NotificationsService::notify`].
    Notify {
        id: u32,
        replaces_id: u32,
        app_name: String,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    },
    /// A `CloseNotification` call. The matching `NotificationClosed(id, 3)`
    /// signal is already emitted on the bus by the time this is sent (see
    /// [`NotificationsService::close_notification`]) — this is purely the
    /// in-process half, for whichever stage first needs to react to a
    /// close by removing a toast or a history entry.
    CloseNotification { id: u32 },
    /// `io.saola.Notifications1`'s `ToggleCentre()`.
    ToggleCentre,
    /// `io.saola.Notifications1`'s `OpenCentre()`.
    OpenCentre,
    /// `io.saola.Notifications1`'s `CloseCentre()`.
    CloseCentre,
    /// `io.saola.Notifications1`'s `SetDnd(b)` — manual DND only; auto-DND
    /// from a live recording (Stage 8) is a separate signal path entirely
    /// and never reaches this file.
    SetDnd { manual: bool },
    /// `io.saola.Notifications1`'s `DismissAll()`.
    DismissAll,
    /// `io.saola.Notifications1`'s `Dismiss(u id)`.
    Dismiss { id: u32 },
}

// ============================================================================
// NotificationsService — org.freedesktop.Notifications.
// ============================================================================

/// The daemon-side implementation of `org.freedesktop.Notifications`.
///
/// Holds only what a served method actually needs: the id counter (see
/// [`IdAllocator`]) and a clone of the channel every method forwards a
/// [`DaemonEvent`] over. `mpsc::Sender` is cheap to clone (an `Arc`-backed
/// handle internally), so every method below clones it fresh rather than
/// threading a `&mut self` through — zbus dispatches concurrent calls with
/// only `&self` available, the same reason [`IdAllocator`] uses an atomic
/// instead of a plain counter.
struct NotificationsService {
    events: mpsc::Sender<DaemonEvent>,
    next_id: IdAllocator,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsService {
    /// `Notify(app_name, replaces_id, app_icon, summary, body, actions,
    /// hints, expire_timeout) -> id`.
    ///
    /// The freedesktop spec fixes this exact eight-argument signature —
    /// `#[allow(clippy::too_many_arguments)]` below is about that fixed
    /// wire contract, not a design choice this file could simplify away.
    ///
    /// `replaces_id != 0` means the caller is asking to reuse that id
    /// (the freedesktop "replace" convention — this crate's own
    /// replace-vs-same-app style rules land in Stage 4); until Stage 4's
    /// store exists to actually replace anything, this just echoes it back
    /// verbatim rather than burning a fresh allocation. `replaces_id == 0`
    /// allocates a new id from [`Self::next_id`].
    ///
    /// Nothing here can fail — id allocation cannot fail, and there is no
    /// argument validation yet (Stage 4's job) — so this returns a plain
    /// `u32`, not a `zbus::fdo::Result`.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.allocate()
        };

        tracing::info!(
            id,
            replaces_id,
            app_name = %app_name,
            summary = %summary,
            hint_count = hints.len(),
            "org.freedesktop.Notifications: Notify"
        );

        if self
            .events
            .clone()
            .try_send(DaemonEvent::Notify {
                id,
                replaces_id,
                app_name,
                app_icon,
                summary,
                body,
                actions,
                hints,
                expire_timeout,
            })
            .is_err()
        {
            tracing::warn!(
                id,
                "org.freedesktop.Notifications: could not forward Notify to the daemon (channel \
                 full or the daemon's event loop is gone) — the caller still gets its id back"
            );
        }

        id
    }

    /// `CloseNotification(id)`. Always emits `NotificationClosed(id, 3)` —
    /// [`store::CloseReason::CloseNotification`], "the
    /// CALL_CLOSE_NOTIFICATION method was called", the
    /// one reason this method is always entitled to claim regardless of
    /// whatever a later stage's store believes about that id (nothing
    /// tracks live notification state yet, so there is nothing to check
    /// against — a `CloseNotification` for an id that never existed still
    /// gets the same honest signal a real freedesktop daemon would send).
    async fn close_notification(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        id: u32,
    ) {
        tracing::info!(id, "org.freedesktop.Notifications: CloseNotification");

        let reason = store::CloseReason::CloseNotification.as_u32();
        if let Err(err) = Self::notification_closed(&emitter, id, reason).await {
            tracing::warn!(
                id,
                error = %err,
                "org.freedesktop.Notifications: could not emit NotificationClosed"
            );
        }

        if self
            .events
            .clone()
            .try_send(DaemonEvent::CloseNotification { id })
            .is_err()
        {
            tracing::warn!(
                id,
                "org.freedesktop.Notifications: could not forward CloseNotification to the \
                 daemon (channel full or the daemon's event loop is gone) — the signal above was \
                 still emitted"
            );
        }
    }

    /// `GetCapabilities() -> as`. See [`capabilities`] for the frozen list.
    async fn get_capabilities(&self) -> Vec<String> {
        capabilities().into_iter().map(String::from).collect()
    }

    /// `GetServerInformation() -> (ssss)`. The four values are frozen by
    /// PLAN.md's Frozen external contracts section — `"1.2"` is the
    /// freedesktop Notifications *spec* version this daemon implements,
    /// not this crate's own `CARGO_PKG_VERSION` (which fills the third
    /// slot instead).
    async fn get_server_information(&self) -> (String, String, String, String) {
        (
            "saola-notifications".to_string(),
            "Saola".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.2".to_string(),
        )
    }

    /// Emitted three ways, all live as of Stage 5: `CloseNotification`
    /// answers its own call with reason `3` (just above), and the toast
    /// surface's expiry and click-dismiss reach this through
    /// [`emit_notification_closed`] with reasons `1` and `2`.
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    /// Emitted from `main.rs`'s `update` via [`emit_action_invoked`] — the
    /// same "no `SignalEmitter` on hand outside a served method" shape
    /// [`notification_closed`] uses for reasons `1`/`2`, applied to Stage
    /// 6's action pills (and a card's own `"default"` action).
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

// ============================================================================
// ControlState — the live snapshot behind io.saola.Notifications1's four
// properties. This is Stage 9's whole design: `ControlService` cannot borrow
// `main.rs`'s `Daemon` (it runs on the zbus object server, dispatched from
// wherever a bus call arrives), so the two sides share a small `Arc<Mutex<_>>`
// snapshot instead — `Daemon::sync_control_state` writes it every time the
// daemon's own state changes, and the property getters below only ever read
// it. Four plain fields behind one lock, not four atomics, because every
// write updates all four together (see `Daemon::control_state_snapshot`) and
// a torn read across four independent atomics could hand a caller a
// `DndActive` computed against a `DndManual` from a different moment.
// ============================================================================

/// A frozen instant of `io.saola.Notifications1`'s four properties. See
/// PLAN.md's Frozen external contracts section for what each one means;
/// `NotificationCount`'s exact definition (history length — the same list
/// the notification centre shows) is pinned in README.md's frozen-contract
/// section, not just here, because it's the one property whose meaning
/// wasn't obvious from its name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlState {
    pub notification_count: u32,
    pub dnd_active: bool,
    pub dnd_manual: bool,
    pub centre_open: bool,
}

/// One of the four `io.saola.Notifications1` properties, named so
/// [`sync_control_state`] can dispatch to the right zbus-generated
/// `<property>_changed` method without stringly-typed matching on the
/// property's wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlProperty {
    NotificationCount,
    DndActive,
    DndManual,
    CentreOpen,
}

impl ControlState {
    /// Which properties differ between `self` (the snapshot as it was) and
    /// `new` (the snapshot as it is now) — pure, so this is the one part of
    /// Stage 9's design worth a table of unit tests rather than only manual
    /// `busctl --user monitor` evidence. The order is fixed (declaration
    /// order) purely so a test can assert an exact `Vec` instead of a set.
    fn changed(&self, new: &ControlState) -> Vec<ControlProperty> {
        let mut changed = Vec::new();
        if self.notification_count != new.notification_count {
            changed.push(ControlProperty::NotificationCount);
        }
        if self.dnd_active != new.dnd_active {
            changed.push(ControlProperty::DndActive);
        }
        if self.dnd_manual != new.dnd_manual {
            changed.push(ControlProperty::DndManual);
        }
        if self.centre_open != new.centre_open {
            changed.push(ControlProperty::CentreOpen);
        }
        changed
    }
}

/// The snapshot [`ControlService`]'s property getters read and
/// [`sync_control_state`] writes. `Arc<Mutex<_>>` rather than a bare
/// `ControlState` because the two sides run on different tasks (the zbus
/// dispatch loop reading, `main.rs`'s `update` writing) with no `&mut`
/// relationship between them — the same reason `dbus.rs`'s served methods
/// hold an `mpsc::Sender` clone rather than a `&mut` channel.
pub type SharedControlState = Arc<Mutex<ControlState>>;

/// Reads the current snapshot, recovering from a poisoned lock rather than
/// panicking — AGENTS.md's no-panic rule, applied defensively: nothing in
/// this crate ever panics while holding this lock (every critical section
/// below is a plain field copy), so poisoning should never actually happen,
/// but `.unwrap()` on the lock result would turn a hypothetical future bug
/// elsewhere into a bus-call panic instead of a stale read.
fn read_control_state(state: &SharedControlState) -> ControlState {
    match state.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Writes `new` into `state` and emits `PropertiesChanged` for whichever of
/// the four properties actually differ from what was there before —
/// PLAN.md Stage 9's "only emit when the value actually changed" rule, via
/// [`ControlState::changed`].
///
/// # Why this needs the connection, not just the state (teaching note)
///
/// Writing `new` into `state` is enough for the *next* `get-property` call
/// to see it — [`ControlService`]'s getters read straight through the
/// `Mutex`. But an already-subscribed caller (`busctl --user monitor`, the
/// saola-panel indicator) only learns about a change that already happened
/// via the `PropertiesChanged` *signal*, which has to be emitted by
/// something holding a `SignalEmitter` for [`CONTROL_OBJECT_PATH`] — the
/// same "no emitter outside a served method" gap [`emit_notification_closed`]
/// exists to close, applied to properties instead of a plain signal.
/// `connection.object_server().interface::<_, ControlService>(path)` is how
/// a caller outside the interface's own methods gets at that emitter (via
/// `InterfaceRef`), exactly the pattern zbus's own `issue_310` regression
/// test uses. Each zbus-generated `<property>_changed` call re-reads its
/// property's getter internally, which is why `state` is written *first* —
/// otherwise the signal would carry the stale value.
pub async fn sync_control_state(
    connection: &Connection,
    state: &SharedControlState,
    new: ControlState,
) -> zbus::Result<Vec<ControlProperty>> {
    let changed = {
        let mut guard = match state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let changed = guard.changed(&new);
        *guard = new;
        changed
    };

    if changed.is_empty() {
        return Ok(changed);
    }

    let iface_ref = connection
        .object_server()
        .interface::<_, ControlService>(CONTROL_OBJECT_PATH)
        .await?;
    let iface = iface_ref.get().await;
    let emitter = iface_ref.signal_emitter();

    for property in &changed {
        match property {
            ControlProperty::NotificationCount => {
                iface.notification_count_changed(emitter).await?;
            }
            ControlProperty::DndActive => iface.dnd_active_changed(emitter).await?,
            ControlProperty::DndManual => iface.dnd_manual_changed(emitter).await?,
            ControlProperty::CentreOpen => iface.centre_open_changed(emitter).await?,
        }
    }

    Ok(changed)
}

// ============================================================================
// ControlService — io.saola.Notifications1.
// ============================================================================

/// The daemon-side implementation of `io.saola.Notifications1`. No id
/// allocator needed here — every method is a simple trigger. `state` is the
/// live snapshot [`Daemon::sync_control_state`] (`main.rs`) writes; the
/// property getters below only ever read it.
struct ControlService {
    events: mpsc::Sender<DaemonEvent>,
    state: SharedControlState,
}

impl ControlService {
    /// Internal helper — a **plain** `impl` block, not `#[zbus::interface]`
    /// (anything inside that macro's block would itself be exported on the
    /// bus, the same reason `saola-capture::dbus::CaptureService`'s own
    /// interactive-region helpers live in a separate plain `impl`). Every
    /// control method below is identically shaped ("log, try_send, warn on
    /// failure"), so this is the one place that shape is written.
    fn forward(&self, event: DaemonEvent, method: &'static str) {
        if self.events.clone().try_send(event).is_err() {
            tracing::warn!(
                method,
                "io.saola.Notifications1: could not forward {method} to the daemon (channel full \
                 or the daemon's event loop is gone)"
            );
        }
    }
}

#[zbus::interface(name = "io.saola.Notifications1")]
impl ControlService {
    async fn toggle_centre(&self) {
        tracing::info!("io.saola.Notifications1: ToggleCentre");
        self.forward(DaemonEvent::ToggleCentre, "ToggleCentre");
    }

    async fn open_centre(&self) {
        tracing::info!("io.saola.Notifications1: OpenCentre");
        self.forward(DaemonEvent::OpenCentre, "OpenCentre");
    }

    async fn close_centre(&self) {
        tracing::info!("io.saola.Notifications1: CloseCentre");
        self.forward(DaemonEvent::CloseCentre, "CloseCentre");
    }

    /// `SetDnd(b)` — manual DND only (Architecture: `effective_dnd = manual
    /// || recording`; recording auto-DND has no bus setter, by design).
    async fn set_dnd(&self, manual: bool) {
        tracing::info!(manual, "io.saola.Notifications1: SetDnd");
        self.forward(DaemonEvent::SetDnd { manual }, "SetDnd");
    }

    async fn dismiss_all(&self) {
        tracing::info!("io.saola.Notifications1: DismissAll");
        self.forward(DaemonEvent::DismissAll, "DismissAll");
    }

    async fn dismiss(&self, id: u32) {
        tracing::info!(id, "io.saola.Notifications1: Dismiss");
        self.forward(DaemonEvent::Dismiss { id }, "Dismiss");
    }

    /// History length — the same list the notification centre shows,
    /// capped at `notifications.toml`'s `history-cap`. See README.md's
    /// frozen-contract section for why this and not the live toast-stack
    /// count is what the panel badge reflects.
    #[zbus(property)]
    fn notification_count(&self) -> u32 {
        read_control_state(&self.state).notification_count
    }

    /// `effective_dnd = manual || recording` (AGENTS.md Architecture) — the
    /// value `main.rs::store::effective_dnd` computes, mirrored here by
    /// [`Daemon::sync_control_state`] every time either half changes.
    #[zbus(property)]
    fn dnd_active(&self) -> bool {
        read_control_state(&self.state).dnd_active
    }

    /// The manual-only DND flag `SetDnd` (above) and the centre's own
    /// toggle both write.
    #[zbus(property)]
    fn dnd_manual(&self) -> bool {
        read_control_state(&self.state).dnd_manual
    }

    /// Whether the notification centre surface is currently mapped.
    #[zbus(property)]
    fn centre_open(&self) -> bool {
        read_control_state(&self.state).centre_open
    }
}

// ============================================================================
// Hint conversion — the only bridge between zbus's wire types and
// store.rs's plain, zbus-free HintValue. store.rs's own module doc comment
// carries the "no zbus imports" rule; this is the one place that rule's
// other half lives — the function that actually reads a
// `zbus::zvariant::OwnedValue` and produces the plain value store.rs
// consumes. Stage 5 calls `hints_to_plain` once per `DaemonEvent::Notify`,
// then hands the result to `store::parse_hints`.
// ============================================================================

/// Converts every hint this crate knows how to parse from
/// `DaemonEvent::Notify`'s raw `hints: HashMap<String, OwnedValue>` into
/// `store.rs`'s own [`store::HintValue`], keyed by the same hint name.
///
/// A hint whose wire value isn't one of the shapes [`hint_value_from_owned`]
/// understands (an `i64` `sender-pid`, a `u32` `x`/`y`, …) is silently
/// dropped from the resulting map rather than erroring — `store::
/// parse_hints` only ever looks up a small fixed set of key names, so a
/// hint this crate has no use for either way just isn't present, exactly as
/// if the sender had never included it.
///
/// Called once per `DaemonEvent::Notify`, from
/// `main.rs::dbus_worker_stream`, before the result is handed to
/// `store::parse_hints`. (Stage 4 shipped this behind a
/// `#[allow(dead_code)]` because nothing called it yet; Stage 5 removed the
/// allow along with the one on [`DaemonEvent`].)
pub fn hints_to_plain(hints: &HashMap<String, OwnedValue>) -> HashMap<String, store::HintValue> {
    hints
        .iter()
        .filter_map(|(key, value)| hint_value_from_owned(value).map(|hv| (key.clone(), hv)))
        .collect()
}

/// Converts one `OwnedValue` into a [`store::HintValue`], or `None` if its
/// wire type isn't one of the four this crate's hint parsing needs: `y`
/// (urgency's byte), `b` (transient/resident), `s` (the image-path
/// aliases), and the `(iiibiiay)` structure (the image-data aliases).
///
/// `OwnedValue: Deref<Target = zvariant::Value<'static>>`, so `&*value`
/// borrows the underlying `Value` for matching without cloning it — cloning
/// a `Value` is fallible in general (`Fd` variants can fail to `dup`, per
/// `zvariant`'s own `try_clone`), so this reads through the reference
/// instead of ever calling `.clone()` on an arbitrary hint value.
fn hint_value_from_owned(value: &OwnedValue) -> Option<store::HintValue> {
    match &**value {
        Value::U8(byte) => Some(store::HintValue::Byte(*byte)),
        Value::Bool(b) => Some(store::HintValue::Bool(*b)),
        Value::Str(s) => Some(store::HintValue::Str(s.as_str().to_string())),
        Value::Structure(structure) => image_data_from_structure(structure),
        _ => None,
    }
}

/// Unpacks the freedesktop `(iiibiiay)` image-data structure
/// (`image-data`/`image_data`/`icon_data`'s wire type) into
/// [`store::HintValue::ImageData`]. Any field with the wrong arity or wire
/// type — a malformed or spec-violating sender — returns `None` rather than
/// panicking on a missing/mismatched field; `store.rs`'s own
/// `decode_image_data` is what validates the *values* (dimensions,
/// rowstride, channel count) once they're plain Rust types.
fn image_data_from_structure(structure: &Structure<'_>) -> Option<store::HintValue> {
    let fields = structure.fields();
    let [
        width,
        height,
        rowstride,
        has_alpha,
        bits_per_sample,
        channels,
        data,
    ] = fields
    else {
        return None;
    };

    Some(store::HintValue::ImageData {
        width: i32_field(width)?,
        height: i32_field(height)?,
        rowstride: i32_field(rowstride)?,
        has_alpha: bool_field(has_alpha)?,
        bits_per_sample: i32_field(bits_per_sample)?,
        channels: i32_field(channels)?,
        data: byte_array_field(data)?,
    })
}

fn i32_field(value: &Value<'_>) -> Option<i32> {
    match value {
        Value::I32(n) => Some(*n),
        _ => None,
    }
}

fn bool_field(value: &Value<'_>) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn byte_array_field(value: &Value<'_>) -> Option<Vec<u8>> {
    match value {
        Value::Array(array) => array
            .iter()
            .map(|element| match element {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

// ============================================================================
// serve / run — name claims and the long-lived worker.
// ============================================================================

/// What [`serve`] settled into.
#[derive(Debug, Clone)]
pub enum ServeOutcome {
    /// This process is serving `io.saola.Notifications1` (and is therefore
    /// the daemon). `notifications_owned` says whether it *also* owns
    /// `org.freedesktop.Notifications` — `false` when another notification
    /// daemon (mako, dunst, …) already does; the control interface and
    /// everything built on it in later stages work either way.
    /// `control_state` is the live snapshot [`ControlService`]'s property
    /// getters read; `main.rs` keeps it and writes every change through
    /// [`sync_control_state`].
    Serving {
        notifications_owned: bool,
        control_state: SharedControlState,
    },
    /// Another `saola-notifications` process already owns
    /// `io.saola.Notifications1` — see this module's "Name claims" doc
    /// comment for what happens next.
    AlreadySecondInstance,
}

/// Registers both interfaces' objects and claims their well-known names.
/// See this module's own doc comment ("Name claims") for the full posture
/// and why the two names are handled differently when already taken.
pub async fn serve(
    connection: &Connection,
    events: mpsc::Sender<DaemonEvent>,
) -> zbus::Result<ServeOutcome> {
    // The live snapshot behind `io.saola.Notifications1`'s four properties —
    // see this module's "ControlState" doc section. Created here (not
    // passed in) because this is the one place `ControlService` itself is
    // constructed; `main.rs` gets its clone back via `ServeOutcome::Serving`.
    let control_state: SharedControlState = Arc::new(Mutex::new(ControlState::default()));

    connection
        .object_server()
        .at(
            NOTIFICATIONS_OBJECT_PATH,
            NotificationsService {
                events: events.clone(),
                next_id: IdAllocator::new(),
            },
        )
        .await?;
    connection
        .object_server()
        .at(
            CONTROL_OBJECT_PATH,
            ControlService {
                events: events.clone(),
                state: control_state.clone(),
            },
        )
        .await?;

    let notifications_owned = match connection
        .request_name_with_flags(
            NOTIFICATIONS_SERVICE_NAME,
            RequestNameFlags::DoNotQueue.into(),
        )
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => true,
        // zbus turns `Exists` into `Err(NameTaken)` before we ever see it
        // as a reply variant, and `InQueue` cannot happen with
        // `DoNotQueue` alone — matching all three keeps this honest
        // rather than relying on that mapping as an implementation
        // detail (same posture as `saola-capture::dbus::serve` and
        // `saola-session::modules::inhibit::ZbusNameClaimant::try_claim`).
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            tracing::info!(
                name = NOTIFICATIONS_SERVICE_NAME,
                "saola-notifications: {NOTIFICATIONS_SERVICE_NAME} is already owned (mako, \
                 dunst, or some other notification daemon) — staying inert on that interface; \
                 the control interface still serves"
            );
            // Nobody will ever call this object (we don't own the name),
            // so take it back down rather than leave an unreachable
            // registration around to confuse introspection — same rule
            // `saola-capture::dbus::serve` follows.
            connection
                .object_server()
                .remove::<NotificationsService, _>(NOTIFICATIONS_OBJECT_PATH)
                .await?;
            false
        }
        Err(err) => return Err(err),
    };

    match connection
        .request_name_with_flags(CONTROL_SERVICE_NAME, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Ok(ServeOutcome::Serving {
                notifications_owned,
                control_state,
            })
        }
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            Ok(ServeOutcome::AlreadySecondInstance)
        }
        Err(err) => Err(err),
    }
}

/// Emits `NotificationClosed(id, reason)` on
/// `org.freedesktop.Notifications` from *outside* a served method.
///
/// # Why this wrapper exists (teaching note)
///
/// `#[zbus(signal)]` declarations inside a `#[zbus::interface]` block expand
/// into associated functions on the handler struct — that is how
/// [`NotificationsService::close_notification`] emits its own reason-`3`
/// signal, using the `SignalEmitter` zbus injects into the method. Reasons
/// `1` (expired) and `2` (user-dismissed) have no method call behind them:
/// they are decided by the UI, in `main.rs`'s `update`, which holds a
/// `Connection` rather than a `&NotificationsService`.
///
/// [`SignalEmitter::new`] is what bridges the two — it binds a connection to
/// an object path, which is everything the generated emitter needs. This
/// function exists so `main.rs` never has to know either the path or the
/// fact that `NotificationsService` is the type carrying that emitter, both
/// of which stay private to this module.
///
/// Errors are returned rather than logged here: the caller is a
/// `Task::future` that already has a warning to attach the id and reason to.
pub async fn emit_notification_closed(
    connection: &Connection,
    id: u32,
    reason: u32,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, NOTIFICATIONS_OBJECT_PATH)?;
    NotificationsService::notification_closed(&emitter, id, reason).await
}

/// Emits `ActionInvoked(id, action_key)` on `org.freedesktop.Notifications`
/// from *outside* a served method — the Stage 6 sibling of
/// [`emit_notification_closed`], same reasoning: an action pill (or a
/// card's own `"default"` action) is invoked from `main.rs`'s `update`, not
/// from a bus call, so there is no `SignalEmitter` already in hand the way
/// a served method gets one for free.
pub async fn emit_action_invoked(
    connection: &Connection,
    id: u32,
    action_key: &str,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, NOTIFICATIONS_OBJECT_PATH)?;
    NotificationsService::action_invoked(&emitter, id, action_key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_starts_at_one_and_increments() {
        let ids = IdAllocator::new();
        assert_eq!(ids.allocate(), 1);
        assert_eq!(ids.allocate(), 2);
        assert_eq!(ids.allocate(), 3);
    }

    /// The load-bearing case: past `u32::MAX`, the next id must be `1`,
    /// never the reserved `0`.
    #[test]
    fn allocate_skips_zero_on_wraparound() {
        let ids = IdAllocator::starting_at(u32::MAX);
        assert_eq!(ids.allocate(), u32::MAX);
        assert_eq!(ids.allocate(), 1);
        assert_eq!(ids.allocate(), 2);
    }

    // ------------------------------------------------------------------
    // hint_value_from_owned / hints_to_plain — the zvariant <-> store.rs
    // HintValue bridge.
    // ------------------------------------------------------------------

    fn owned(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).expect("simple scalar/structure values always convert")
    }

    #[test]
    fn byte_value_converts_to_hint_byte() {
        assert_eq!(
            hint_value_from_owned(&owned(Value::U8(2))),
            Some(store::HintValue::Byte(2))
        );
    }

    #[test]
    fn bool_value_converts_to_hint_bool() {
        assert_eq!(
            hint_value_from_owned(&owned(Value::Bool(true))),
            Some(store::HintValue::Bool(true))
        );
    }

    #[test]
    fn str_value_converts_to_hint_str() {
        assert_eq!(
            hint_value_from_owned(&owned(Value::Str("/tmp/icon.png".into()))),
            Some(store::HintValue::Str("/tmp/icon.png".to_string()))
        );
    }

    #[test]
    fn structure_value_converts_to_hint_image_data() {
        let structure = Structure::from((1i32, 1i32, 3i32, false, 8i32, 3i32, vec![1u8, 2, 3]));
        let converted = hint_value_from_owned(&owned(Value::Structure(structure)));
        assert_eq!(
            converted,
            Some(store::HintValue::ImageData {
                width: 1,
                height: 1,
                rowstride: 3,
                has_alpha: false,
                bits_per_sample: 8,
                channels: 3,
                data: vec![1, 2, 3],
            })
        );
    }

    #[test]
    fn structure_with_wrong_arity_does_not_convert() {
        let structure = Structure::from((1i32, 2i32));
        assert_eq!(
            hint_value_from_owned(&owned(Value::Structure(structure))),
            None
        );
    }

    #[test]
    fn unsupported_value_type_does_not_convert() {
        // sender-pid's real wire type — this crate has no HintValue variant
        // for it, and never will need one.
        assert_eq!(hint_value_from_owned(&owned(Value::I64(4242))), None);
    }

    #[test]
    fn hints_to_plain_drops_unconvertible_entries_and_keeps_the_rest() {
        let mut hints = HashMap::new();
        hints.insert("urgency".to_string(), owned(Value::U8(2)));
        hints.insert("sender-pid".to_string(), owned(Value::I64(4242)));

        let plain = hints_to_plain(&hints);
        assert_eq!(plain.len(), 1);
        assert_eq!(plain.get("urgency"), Some(&store::HintValue::Byte(2)));
    }

    #[test]
    fn capabilities_matches_the_frozen_contract() {
        assert_eq!(
            capabilities(),
            vec!["body", "actions", "icon-static", "persistence"]
        );
    }

    // ------------------------------------------------------------------
    // ControlState::changed — the pure diff Stage 9's PropertiesChanged
    // emission is gated on.
    // ------------------------------------------------------------------

    #[test]
    fn an_unchanged_snapshot_reports_nothing_changed() {
        let state = ControlState {
            notification_count: 3,
            dnd_active: true,
            dnd_manual: true,
            centre_open: false,
        };
        assert!(state.changed(&state).is_empty());
    }

    #[test]
    fn notification_count_alone_is_reported_alone() {
        let before = ControlState::default();
        let after = ControlState {
            notification_count: 1,
            ..before
        };
        assert_eq!(
            before.changed(&after),
            vec![ControlProperty::NotificationCount]
        );
    }

    #[test]
    fn dnd_active_alone_is_reported_alone() {
        let before = ControlState::default();
        let after = ControlState {
            dnd_active: true,
            ..before
        };
        assert_eq!(before.changed(&after), vec![ControlProperty::DndActive]);
    }

    #[test]
    fn dnd_manual_alone_is_reported_alone() {
        let before = ControlState::default();
        let after = ControlState {
            dnd_manual: true,
            ..before
        };
        assert_eq!(before.changed(&after), vec![ControlProperty::DndManual]);
    }

    #[test]
    fn centre_open_alone_is_reported_alone() {
        let before = ControlState::default();
        let after = ControlState {
            centre_open: true,
            ..before
        };
        assert_eq!(before.changed(&after), vec![ControlProperty::CentreOpen]);
    }

    /// Recording starting flips `dnd_manual: false` into `dnd_active: true`
    /// in one snapshot — the two-properties-in-one-write case Stage 8's
    /// `set_recording` produces.
    #[test]
    fn two_properties_changing_together_are_both_reported_in_declaration_order() {
        let before = ControlState::default();
        let after = ControlState {
            notification_count: 1,
            dnd_active: true,
            ..before
        };
        assert_eq!(
            before.changed(&after),
            vec![
                ControlProperty::NotificationCount,
                ControlProperty::DndActive
            ]
        );
    }

    #[test]
    fn every_property_changing_is_reported_in_declaration_order() {
        let before = ControlState::default();
        let after = ControlState {
            notification_count: 5,
            dnd_active: true,
            dnd_manual: true,
            centre_open: true,
        };
        assert_eq!(
            before.changed(&after),
            vec![
                ControlProperty::NotificationCount,
                ControlProperty::DndActive,
                ControlProperty::DndManual,
                ControlProperty::CentreOpen,
            ]
        );
    }

    #[test]
    fn read_control_state_returns_whats_written() {
        let state: SharedControlState = Arc::new(Mutex::new(ControlState {
            notification_count: 7,
            dnd_active: true,
            dnd_manual: false,
            centre_open: true,
        }));
        assert_eq!(
            read_control_state(&state),
            ControlState {
                notification_count: 7,
                dnd_active: true,
                dnd_manual: false,
                centre_open: true,
            }
        );
    }
}
