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
//! [`serve`] is this module's only entry point; [`emit_notification_closed`]
//! is its only exit.
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

    /// Part of the frozen contract from this stage on; nothing emits it
    /// yet — Stage 6 ("Actions") is what invokes an action pill and fires
    /// this.
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

// ============================================================================
// ControlService — io.saola.Notifications1.
// ============================================================================

/// The daemon-side implementation of `io.saola.Notifications1`. No id
/// allocator needed here — every method is a simple trigger.
struct ControlService {
    events: mpsc::Sender<DaemonEvent>,
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

    /// Placeholder — no store exists yet to count against. Stage 9 wires
    /// this (and the three properties below) to live state and starts
    /// emitting `PropertiesChanged`, per PLAN.md's Frozen external
    /// contracts section ("all properties emit `PropertiesChanged`").
    #[zbus(property)]
    fn notification_count(&self) -> u32 {
        0
    }

    /// Placeholder for `effective_dnd = manual || recording` (Architecture)
    /// — always `false` until Stage 8/9 wire real DND state.
    #[zbus(property)]
    fn dnd_active(&self) -> bool {
        false
    }

    /// Placeholder for the manual-only DND flag `SetDnd` above will
    /// eventually toggle.
    #[zbus(property)]
    fn dnd_manual(&self) -> bool {
        false
    }

    /// Placeholder — no centre surface exists until Stage 7.
    #[zbus(property)]
    fn centre_open(&self) -> bool {
        false
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    /// This process is serving `io.saola.Notifications1` (and is therefore
    /// the daemon). `notifications_owned` says whether it *also* owns
    /// `org.freedesktop.Notifications` — `false` when another notification
    /// daemon (mako, dunst, …) already does; the control interface and
    /// everything built on it in later stages work either way.
    Serving { notifications_owned: bool },
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
}
