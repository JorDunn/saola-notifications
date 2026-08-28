//! The bridge to `io.saola.Capture1`: native toasts for a screenshot, a
//! saved recording or a capture-side failure, and the auto-DND that
//! suppresses toasts (all of them, including critical) for the duration of
//! a live recording.
//!
//! # No proxy — a `MatchRule`, like `saola-panel::modules::claude` (teaching
//! note)
//!
//! Every other zbus module this crate has (`dbus.rs`'s own served
//! interfaces) either serves an object or, when it consumes one, does so
//! with a `#[zbus::proxy]`-generated struct built around a stable
//! destination and path. That fits `io.saola.Capture1` fine in principle —
//! it *is* a stable, well-known bus name — but this bridge only ever wants
//! *signals*, and a generated proxy's signal streams are one more layer
//! than a bare `zbus::MatchRule` + `zbus::MessageStream::for_match_rule`
//! needs: no method calls, no cached properties, nothing to keep in sync.
//! PLAN.md's own Stage 8 task names this shape explicitly and points at
//! `saola-panel::modules::claude` for it — that module's own doc comment
//! has the fuller "why not a proxy" case; the short version is the same
//! one that applies here: absence is silent for free, because a
//! `MatchRule` is always valid to register whether or not `saola-capture`
//! is running at all (this crate never fails to boot, or even logs a
//! warning, for a capture daemon that never shows up — the four signals
//! and the ownership watch below just never fire).
//!
//! # The bus schema (frozen by `saola-capture`, copied verbatim)
//!
//! Verified against `/home/jordan/Developer/saola-capture/src/dbus.rs`:
//! bus name and interface name `io.saola.Capture1`
//! ([`CAPTURE_SERVICE_NAME`]/[`CAPTURE_INTERFACE`] — the freedesktop
//! convention of interface name == bus name for a service's own primary
//! interface), object path `/io/saola/Capture1` ([`CAPTURE_OBJECT_PATH`]),
//! signals `CaptureTaken(path: s, kind: s)`, `RecordingStarted(kind: s)`,
//! `RecordingFinished(path: s)`, `Error(message: s)`.
//!
//! # `Error` is also a recording terminator (a finding, not PLAN.md's own
//! words)
//!
//! PLAN.md's task prose says `RecordingStarted` turns auto-DND on and
//! `RecordingFinished` turns it off, and treats `Error` only as "→ toast".
//! Reading `saola-capture::dbus`'s own recording-finalization code
//! (`spawn_recording_tasks`'s async tail, around its `SignalEmitter::new`
//! match) shows those two outcomes are **mutually exclusive**: a recording
//! that ends cleanly emits `RecordingFinished(path)`; one that ends any
//! other way — the encoder dying, a full disk, the cast collapsing — emits
//! `Error(message)` **instead**, never both. If this bridge only cleared
//! auto-DND on `RecordingFinished`, a single failed recording would leave
//! auto-DND stuck on forever, with nothing to clear it short of
//! `saola-capture` itself vanishing (this file's leak guard, below) — a
//! real, permanent-suppression bug, not a hypothetical one. So `Error` folds
//! into the exact same "recording ended" transition as `RecordingFinished`
//! ([`RecordingState::on_finished`]) before its own toast is pushed; see
//! [`error_toast`]'s doc comment for the corresponding title choice.
//!
//! # The DND leak guard
//!
//! `NameOwnerChanged` (the bus daemon's own signal, `org.freedesktop.DBus`
//! at `/org/freedesktop/DBus`) is watched with a bus-side `arg0` filter on
//! [`CAPTURE_SERVICE_NAME`], the same "peer-vanish cleanup" shape
//! `saola-session::modules::inhibit::watch_for_vanished_peers` uses for its
//! own crashed-peer cleanup (that module's doc comment has the fuller
//! rationale). Unlike that module's arg-2-only filter (it only cares that
//! *some* peer released a cookie-holding name), this one filters on arg 0
//! — the name itself — because it needs to know about *any* change of
//! `io.saola.Capture1`'s owner, not only the "nobody owns it now" case: a
//! crashed `saola-capture` immediately replaced by a fresh instance (still
//! `DoNotQueue`, so there is a real gap, but a supervisor could restart it
//! fast enough to matter) is exactly as much "the process that was
//! recording is gone" as a bus name going fully unowned. [`decode_owner_change`]
//! is where that distinction is actually drawn (a *fresh* claim from
//! nothing — `old_owner` empty — is not a vanish; see its own doc comment).
//!
//! # The id space (a decision PLAN.md left open, made and documented here)
//!
//! A capture-native toast still needs a real id — `NotificationClosed`
//! fires for it exactly like any other toast (expiry, click-dismiss,
//! `DismissAll`), and that reason code needs an id nothing else is using.
//! PLAN.md offered two shapes: share `dbus.rs`'s own bus-facing
//! `IdAllocator`, or something "store-side". Sharing that allocator would
//! mean threading a value across two independently-spawned
//! `iced::Subscription`s (this bridge's own worker and
//! `main.rs::dbus_worker_stream` run as separate tasks with no shared state
//! by default — `Subscription::run_with`'s dedup key would need `Arc<..>:
//! Hash`, which `dbus::IdAllocator` doesn't implement, and inventing a
//! pointer-hashing newtype purely to satisfy that felt like more machinery
//! than the problem needs), and it would mean touching `dbus.rs`'s frozen,
//! already-tested `Notify`-id allocation code for a feature that has
//! nothing to do with `Notify`. Instead: [`NativeIdAllocator`] reserves the
//! **upper half** of the `u32` id space (`NATIVE_ID_FLOOR` upward) purely
//! by construction — no shared state, no risk to Stage 3's frozen
//! allocator, and a bus client would need to receive over two billion
//! sequential ids before the two ranges could ever meet. It lives on
//! `Daemon` (`main.rs`), not here, because id allocation for a toast that
//! is about to be pushed through `Store` happens in exactly one place
//! already — `Daemon::push_notification`'s caller — and this bridge's own
//! [`Message`] variants never need to know their own id at all.
//!
//! # No view (a third deviation from `modules/mod.rs`'s documented two)
//!
//! This module is a background bridge, not a surface — it maps no
//! layer-shell window and has nothing to render, ever (PLAN.md's own task
//! prose: "renders nothing"). So unlike [`super::toast`]/[`super::centre`]
//! there is no state struct and no `view` here at all: [`subscription`] is
//! a free function, and everything this bridge needs kept around between
//! events ([`RecordingState`], [`NativeIdAllocator`]) lives on `Daemon`
//! itself, because both are genuinely shared with `store.rs`'s DND policy
//! and `main.rs`'s own id-allocation site — neither is private view state
//! the way `toast::Toasts`'s hovered-card tracking is.

use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::image;
use saola_theme::Theme;
use zbus::{Connection, MatchRule, MessageStream};

use crate::store;

// ============================================================================
// The frozen `io.saola.Capture1` wire shape — copied, not guessed, from
// `/home/jordan/Developer/saola-capture/src/dbus.rs`.
// ============================================================================

const CAPTURE_SERVICE_NAME: &str = "io.saola.Capture1";
const CAPTURE_OBJECT_PATH: &str = "/io/saola/Capture1";
/// Also `"io.saola.Capture1"` — see the module doc comment's "bus schema"
/// section for why this is a separate constant from
/// [`CAPTURE_SERVICE_NAME`] even though the two strings are identical
/// today (a `.destination`-shaped name claim and an `.interface`-shaped
/// match-rule filter are different uses of the same freedesktop
/// convention, not the same fact spelled twice by accident).
const CAPTURE_INTERFACE: &str = "io.saola.Capture1";

const DBUS_DAEMON_OBJECT_PATH: &str = "/org/freedesktop/DBus";
const DBUS_DAEMON_INTERFACE: &str = "org.freedesktop.DBus";
const NAME_OWNER_CHANGED_MEMBER: &str = "NameOwnerChanged";

// ============================================================================
// RecordingState — the pure auto-DND transition table (PLAN.md Stage 8:
// "a small tested state machine"). No zbus, no clock, no `Daemon`.
// ============================================================================

/// Auto-DND's own state, driven only by [`Message::RecordingStarted`],
/// [`Message::RecordingFinished`]/[`Message::Error`] (see the module doc
/// comment for why the two fold into the same transition), and
/// [`Message::CaptureVanished`] (the leak guard). Pure and total — every
/// method takes `self` by value and returns the next state, so every
/// ordering PLAN.md Stage 8 asks to be tested (started/finished/vanished,
/// in any order, including "finished never arrives") is just a sequence of
/// calls against this type, with no bus and no clock required. See this
/// module's tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingState {
    #[default]
    Idle,
    Recording,
}

impl RecordingState {
    /// Whether auto-DND should currently be on because of this state —
    /// `Daemon::recording_dnd`'s source of truth
    /// (`store::effective_dnd = manual || recording`, AGENTS.md).
    pub fn is_active(self) -> bool {
        matches!(self, Self::Recording)
    }

    /// `RecordingStarted`. Always lands on `Recording`, regardless of the
    /// state coming in — a second `RecordingStarted` while already
    /// recording (a duplicate signal, or one racing a not-yet-processed
    /// `RecordingFinished` from the *previous* recording) just confirms
    /// "yes, recording now", which was already true either way.
    pub fn on_started(self) -> Self {
        Self::Recording
    }

    /// `RecordingFinished`, or `Error` (see the module doc comment's
    /// "`Error` is also a recording terminator" finding). Always lands on
    /// `Idle`, including when it arrives with nothing currently recording
    /// (a stray or duplicate signal) — that case is simply a no-op.
    pub fn on_finished(self) -> Self {
        Self::Idle
    }

    /// The `NameOwnerChanged` leak guard: capture's ownership of
    /// `io.saola.Capture1` changed away from what it was. Always lands on
    /// `Idle`. The returned `bool` is whether this actually cleared a
    /// *live* auto-DND (`self` was `Recording`) — the caller uses it to
    /// decide whether "the leak guard fired" is worth a warning log, per
    /// AGENTS.md's absent-service-degrades-silently rule: a vanish while
    /// already idle (capture exited cleanly after its own
    /// `RecordingFinished`/`Error` already ran, or was simply never
    /// recording) is not a leak and should not be reported as though one
    /// occurred.
    pub fn on_vanished(self) -> (Self, bool) {
        (Self::Idle, self == Self::Recording)
    }
}

// ============================================================================
// NativeIdAllocator — ids for capture-native toasts. See the module doc
// comment's "The id space" section for why this exists instead of sharing
// `dbus.rs`'s own allocator.
// ============================================================================

/// The floor of the id range reserved for notifications this daemon
/// invents itself, rather than an app asking for one over `Notify`.
/// `dbus::IdAllocator` (the bus-facing allocator, whose "`AtomicU32`, start
/// 1, skip 0 on wrap" shape is frozen by PLAN.md's Architecture section and
/// pinned by its own Stage 3 tests) is never touched by this file.
const NATIVE_ID_FLOOR: u32 = 0x8000_0000;

/// Allocates ids for capture-native toasts. Plain (non-atomic) on purpose —
/// unlike `dbus::IdAllocator`, which `#[zbus::interface]` methods can call
/// under concurrent dispatch and so must defend with an atomic, this type
/// is only ever touched from `Daemon::update` (`main.rs`), which iced calls
/// single-threaded. There is no concurrent caller here to defend against.
#[derive(Debug)]
pub struct NativeIdAllocator {
    next: u32,
}

impl Default for NativeIdAllocator {
    fn default() -> Self {
        Self {
            next: NATIVE_ID_FLOOR,
        }
    }
}

impl NativeIdAllocator {
    /// Test-only: start the counter somewhere other than the floor, so
    /// wraparound can be exercised directly.
    #[cfg(test)]
    fn starting_at(next: u32) -> Self {
        Self { next }
    }

    /// Allocates and returns the next id, wrapping past `u32::MAX` back to
    /// [`NATIVE_ID_FLOOR`] rather than `0` — every value this ever returns
    /// stays in the reserved high half, including across a wrap.
    pub fn allocate(&mut self) -> u32 {
        let id = self.next;
        self.next = if id == u32::MAX {
            NATIVE_ID_FLOOR
        } else {
            id + 1
        };
        id
    }
}

// ============================================================================
// NativeToast — the pure "what does this event's toast say" half. No zbus,
// no id, no clock; `main.rs` supplies those when it builds a real
// `store::Notification` from one of these.
// ============================================================================

/// One capture-native event's worth of toast content, with everything
/// resolved except the id and the clock — `main.rs::NotifyRequest` carries
/// the identical split for a bus `Notify`, and for the identical reason
/// (the Stage 5 handoff: `posted_at` is stamped once, in `update`, nowhere
/// else).
#[derive(Debug, Clone, PartialEq)]
pub struct NativeToast {
    pub summary: String,
    pub body: String,
    pub image: Option<image::Handle>,
}

/// The app name every capture-native toast carries: `"saola-capture"`,
/// verbatim — the literal string
/// `saola-capture::modules::toast::card_view`'s own header renders in the
/// same slot (that crate's own toasts are entirely local; this is the only
/// place that string exists outside it). PLAN.md Stage 8: "native toast
/// (summary/body/icon per capture's own interim toasts)".
pub const APP_NAME: &str = "saola-capture";

/// `CaptureTaken(path, kind)`. `kind` (one of `"fullscreen"`/`"region"`/
/// `"window"`, echoed back from whatever `Screenshot` was called with) is
/// deliberately not part of the toast copy — capture's own `card_view`
/// never varies its "Screenshot saved" title by kind either
/// (`saola-capture/src/modules/toast.rs`'s `ToastKind::Capture` arm).
pub fn screenshot_toast(path: &str, image: Option<image::Handle>) -> NativeToast {
    NativeToast {
        summary: "Screenshot saved".to_string(),
        body: file_name_or_full_path(path),
        image,
    }
}

/// `RecordingFinished(path)`. No thumbnail is attempted — a saved
/// recording is a video file, not something the Stage 4 PNG decoder can
/// read — matching capture's own `ToastKind::Recording` arm, which renders
/// a plain ivory tile with no glyph.
pub fn recording_toast(path: &str) -> NativeToast {
    NativeToast {
        summary: "Recording saved".to_string(),
        body: file_name_or_full_path(path),
        image: None,
    }
}

/// `Error(message)`. The signal's own doc comment (in
/// `saola-capture/src/dbus.rs`) describes it as generic — "a user-facing
/// failure ... after a method already returned success" — even though its
/// only emitter today is a recording that died without ever reaching
/// `RecordingFinished` (see the module doc comment's "`Error` is also a
/// recording terminator" finding). `"Capture error"` is this bridge's own
/// choice of title for that generic signal, not a copy of any existing
/// capture string: capture's own *local* reaction to the identical
/// internal event is titled `"Recording failed"` (`saola-capture/src/
/// main.rs`'s `Message::RecordingFailed` arm), but hardcoding that title
/// here would misdescribe a future emitter this signal's own doc comment
/// already anticipates.
pub fn error_toast(message: &str) -> NativeToast {
    NativeToast {
        summary: "Capture error".to_string(),
        body: message.to_string(),
        image: None,
    }
}

/// Shared by [`screenshot_toast`] and [`recording_toast`]: the saved
/// file's own name, or the whole string if it has none (a bare filename
/// with no directory component, or one of the paths `std::path::Path` also
/// has no `file_name` for — `""`, `"."`, `".."`, a trailing-slash
/// directory). Ports capture's own
/// `path.file_name().map(...).unwrap_or_else(|| path.display().to_string())`
/// (`saola-capture/src/modules/toast.rs`'s `card_view`) from an owned
/// `PathBuf` to a plain `&str` — this bridge never needs an owned path for
/// anything else.
fn file_name_or_full_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

// ============================================================================
// Message — what this bridge hands `main.rs`.
// ============================================================================

/// One `io.saola.Capture1` event, or the leak guard firing. `main.rs`
/// nests this as `Message::CaptureBridge(capture_bridge::Message)` and
/// handles every variant in `Daemon::on_capture_bridge`.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `CaptureTaken(path, kind)`, with the thumbnail already decoded
    /// (sync, size-bounded, failure → `None`) — see [`decode_capture_signal`]
    /// for where that happens, and why it happens in the worker rather
    /// than in `update` (the same "decode is blocking I/O, keep it off the
    /// update thread" reasoning `main.rs::NotifyRequest`'s own doc comment
    /// gives for a bus `Notify`'s `image-path` hint).
    CaptureTaken {
        path: String,
        image: Option<image::Handle>,
    },
    /// `RecordingStarted(kind)`. `kind` is carried only for the log line
    /// `Daemon::set_recording` writes — nothing branches on it.
    RecordingStarted { kind: String },
    /// `RecordingFinished(path)`.
    RecordingFinished { path: String },
    /// `Error(message)`.
    Error { message: String },
    /// The DND leak guard: `NameOwnerChanged` reported
    /// [`CAPTURE_SERVICE_NAME`]'s owner actually changing away from what
    /// it was. Carries nothing — [`RecordingState::on_vanished`] is all
    /// `main.rs` needs to react correctly.
    CaptureVanished,
}

// ============================================================================
// The subscription and its worker.
// ============================================================================

/// This bridge as an `iced::Subscription`. Unconditional — see the module
/// doc comment's "no proxy" section for why registering the match rules
/// below never fails just because `saola-capture` isn't running.
pub fn subscription() -> Subscription<Message> {
    Subscription::run(capture_bridge_stream)
}

/// Builds the async stream the subscription runs. Every failure path here
/// — no session bus, either match rule failing to register, either signal
/// stream ending — funnels into "the worker ends quietly": unlike
/// `saola-panel::modules::claude`'s own `claude_code_stream`, this bridge
/// holds no model of its own to reset on failure (no `Sessions::default()`
/// equivalent to send) — `Daemon`'s own [`RecordingState`] already defaults
/// to `Idle`, which is exactly what "capture is not there" should look
/// like, and there is no third state a lost connection needs to signal.
fn capture_bridge_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        let _ = watch_capture(&mut sender).await;
    })
}

/// The worker proper: register both match rules, then relay every matching
/// signal forever. Modeled on `saola-panel::modules::claude::
/// watch_claude_code` (one interface-wide `MatchRule`, dispatch on member
/// name) plus a second `MatchRule` for the `NameOwnerChanged` leak guard,
/// merged with `tokio::select!` — the same two-branch shape
/// `saola-session::modules::inhibit::watch_for_vanished_peers` uses for its
/// own shutdown-plus-signal select, minus the shutdown branch (this
/// bridge's own lifetime is the connection's, exactly like
/// `main.rs::dbus_worker_stream`).
async fn watch_capture(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // Session bus: `saola-capture` is a per-user daemon, not a system
    // service — same reasoning as every other bus-consuming module in this
    // family.
    let connection = Connection::session().await?;

    let signal_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(CAPTURE_OBJECT_PATH)?
        .interface(CAPTURE_INTERFACE)?
        .build();
    let mut signals = MessageStream::for_match_rule(signal_rule, &connection, Some(8)).await?;

    // Filtered bus-side to `arg0 == CAPTURE_SERVICE_NAME` — see the module
    // doc comment's "The DND leak guard" section for why this watches the
    // name itself rather than only "new_owner is empty" the way
    // `saola-session`'s peer-vanish watcher does.
    let owner_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(DBUS_DAEMON_OBJECT_PATH)?
        .interface(DBUS_DAEMON_INTERFACE)?
        .member(NAME_OWNER_CHANGED_MEMBER)?
        .arg(0, CAPTURE_SERVICE_NAME)?
        .build();
    let mut owner_changes = MessageStream::for_match_rule(owner_rule, &connection, Some(4)).await?;

    // Read once, here, rather than per event — `main.rs::dbus_worker_stream`
    // does the identical thing for the identical reason: `CaptureTaken`'s
    // thumbnail decode needs the icon-tile size, and this worker has no
    // access to `Daemon`'s own `Theme`. `Theme::saola()` is a constant of
    // the design system, so the two can never disagree.
    let icon_tile = Theme::saola().sizes.icon_tile;

    loop {
        tokio::select! {
            message = signals.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let Ok(message) = message else {
                    continue;
                };
                let Some(event) = decode_capture_signal(&message, icon_tile) else {
                    continue;
                };
                if sender.send(event).await.is_err() {
                    return Ok(());
                }
            }
            message = owner_changes.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let Ok(message) = message else {
                    continue;
                };
                let Some(event) = decode_owner_change(&message) else {
                    continue;
                };
                if sender.send(event).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// Decodes one message already known (by the match rule that delivered it)
/// to be a signal on `io.saola.Capture1` at `/io/saola/Capture1`, or `None`
/// for a member this crate doesn't know (a future capture signal this
/// build predates) or a body that doesn't match the frozen signature (a
/// hand-typed `busctl emit` with the wrong types) — the same skip-don't-die
/// posture `saola-panel::modules::claude::fold` uses for an unrecognized
/// status string.
///
/// A free function taking the real `&zbus::Message` (rather than loose
/// strings) so the tests below can exercise the *real* parse, wrong-body
/// rejection included, against messages built with `zbus::Message::signal`
/// — no bus required, the same technique `claude.rs`'s own `parse_usage`
/// tests use.
fn decode_capture_signal(message: &zbus::Message, icon_tile: f32) -> Option<Message> {
    match message.header().member().map(|member| member.as_str()) {
        Some("CaptureTaken") => {
            let (path, _kind) = message.body().deserialize::<(String, String)>().ok()?;
            let image = store::decode_path_str(&path, icon_tile);
            Some(Message::CaptureTaken { path, image })
        }
        Some("RecordingStarted") => {
            let (kind,) = message.body().deserialize::<(String,)>().ok()?;
            Some(Message::RecordingStarted { kind })
        }
        Some("RecordingFinished") => {
            let (path,) = message.body().deserialize::<(String,)>().ok()?;
            Some(Message::RecordingFinished { path })
        }
        Some("Error") => {
            let (text,) = message.body().deserialize::<(String,)>().ok()?;
            Some(Message::Error { message: text })
        }
        _ => None,
    }
}

/// Decodes one `NameOwnerChanged(name, old_owner, new_owner)` message
/// already filtered (bus-side) to `arg0 == CAPTURE_SERVICE_NAME`, into
/// [`Message::CaptureVanished`] — or `None` when nothing actually changed
/// *away* from a real owner.
///
/// `old_owner.is_empty()` means the name had no owner before this event —
/// a fresh `saola-capture` claiming it from nothing, not a vanish, so it is
/// skipped. `old_owner == new_owner` cannot really happen (the bus daemon
/// only emits this signal on an actual ownership change) but is checked
/// anyway rather than assumed, matching this crate's own "no unvalidated
/// assumptions" rule. Anything else — `new_owner` empty (nobody owns it
/// now) or `new_owner` a *different* owner than before — is a real change
/// away from whichever process used to hold the name, which is exactly
/// what the leak guard exists to catch.
fn decode_owner_change(message: &zbus::Message) -> Option<Message> {
    let (_name, old_owner, new_owner) = message
        .body()
        .deserialize::<(String, String, String)>()
        .ok()?;
    if old_owner.is_empty() || old_owner == new_owner {
        return None;
    }
    Some(Message::CaptureVanished)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // RecordingState — every ordering PLAN.md Stage 8 asks for.
    // ------------------------------------------------------------------

    #[test]
    fn idle_is_the_default() {
        assert_eq!(RecordingState::default(), RecordingState::Idle);
        assert!(!RecordingState::default().is_active());
    }

    #[test]
    fn started_from_idle_is_recording() {
        assert_eq!(RecordingState::Idle.on_started(), RecordingState::Recording);
        assert!(RecordingState::Recording.is_active());
    }

    #[test]
    fn a_second_started_while_already_recording_stays_recording() {
        assert_eq!(
            RecordingState::Recording.on_started(),
            RecordingState::Recording,
            "a duplicate or racing RecordingStarted just confirms what was already true"
        );
    }

    #[test]
    fn finished_from_recording_is_idle() {
        assert_eq!(
            RecordingState::Recording.on_finished(),
            RecordingState::Idle
        );
    }

    #[test]
    fn finished_with_nothing_recording_is_a_harmless_no_op() {
        assert_eq!(
            RecordingState::Idle.on_finished(),
            RecordingState::Idle,
            "a stray or duplicate RecordingFinished/Error must not panic or do anything odd"
        );
    }

    #[test]
    fn a_recording_that_never_finishes_is_cleared_by_the_vanish_leak_guard() {
        // PLAN.md Stage 8: "including finish-never-arrives" — Started, then
        // no RecordingFinished/Error ever comes, but capture itself
        // vanishes. The leak guard is what recovers auto-DND, not a
        // RecordingFinished that never arrives.
        let (state, leaked) = RecordingState::Idle.on_started().on_vanished();
        assert_eq!(state, RecordingState::Idle);
        assert!(
            leaked,
            "this genuinely was a live auto-DND the guard had to clear"
        );
    }

    #[test]
    fn vanished_while_idle_reports_no_leak() {
        let (state, leaked) = RecordingState::Idle.on_vanished();
        assert_eq!(state, RecordingState::Idle);
        assert!(
            !leaked,
            "capture was never recording (or already finished cleanly) — nothing leaked"
        );
    }

    #[test]
    fn double_vanish_is_idempotent_and_never_reports_a_leak_twice() {
        let (state, _) = RecordingState::Idle.on_vanished();
        let (state, leaked) = state.on_vanished();
        assert_eq!(state, RecordingState::Idle);
        assert!(!leaked);
    }

    #[test]
    fn the_full_started_finished_sequence_ends_idle() {
        let state = RecordingState::Idle.on_started().on_finished();
        assert_eq!(state, RecordingState::Idle);
    }

    #[test]
    fn a_finished_arriving_after_the_leak_guard_already_cleared_it_is_a_no_op() {
        // Started, then Vanished (capture died mid-recording, leak guard
        // fires), then a late RecordingFinished/Error the dying process
        // still managed to emit arrives afterward. Must not resurrect
        // anything or double-clear.
        let (state, leaked) = RecordingState::Idle.on_started().on_vanished();
        assert!(leaked);
        let state = state.on_finished();
        assert_eq!(state, RecordingState::Idle);
    }

    #[test]
    fn started_then_finished_then_vanished_reports_no_leak() {
        // The ordinary happy path, then capture exits normally sometime
        // later — the vanish must not be misreported as a leak.
        let state = RecordingState::Idle.on_started().on_finished();
        let (state, leaked) = state.on_vanished();
        assert_eq!(state, RecordingState::Idle);
        assert!(!leaked);
    }

    #[test]
    fn a_second_recording_after_a_clean_finish_starts_fresh() {
        let state = RecordingState::Idle.on_started().on_finished().on_started();
        assert_eq!(state, RecordingState::Recording);
    }

    // ------------------------------------------------------------------
    // NativeIdAllocator
    // ------------------------------------------------------------------

    #[test]
    fn the_first_allocation_is_the_reserved_floor() {
        let mut ids = NativeIdAllocator::default();
        assert_eq!(ids.allocate(), NATIVE_ID_FLOOR);
    }

    #[test]
    fn allocations_increment() {
        let mut ids = NativeIdAllocator::default();
        assert_eq!(ids.allocate(), NATIVE_ID_FLOOR);
        assert_eq!(ids.allocate(), NATIVE_ID_FLOOR + 1);
        assert_eq!(ids.allocate(), NATIVE_ID_FLOOR + 2);
    }

    #[test]
    fn wraparound_lands_back_on_the_floor_never_on_zero_or_the_low_half() {
        let mut ids = NativeIdAllocator::starting_at(u32::MAX);
        assert_eq!(ids.allocate(), u32::MAX);
        assert_eq!(
            ids.allocate(),
            NATIVE_ID_FLOOR,
            "wraps back into the reserved high half, not to 0 and not to 1"
        );
    }

    #[test]
    fn every_allocation_around_a_wrap_stays_in_the_high_half() {
        let mut ids = NativeIdAllocator::starting_at(u32::MAX - 1);
        for _ in 0..5 {
            assert!(ids.allocate() >= NATIVE_ID_FLOOR);
        }
    }

    // ------------------------------------------------------------------
    // NativeToast copy — screenshot / recording / error.
    // ------------------------------------------------------------------

    #[test]
    fn screenshot_toast_titles_and_names_the_file() {
        let toast = screenshot_toast("/home/jordan/Pictures/Screenshot_2026-08-28.png", None);
        assert_eq!(toast.summary, "Screenshot saved");
        assert_eq!(toast.body, "Screenshot_2026-08-28.png");
        assert_eq!(toast.image, None);
    }

    #[test]
    fn screenshot_toast_carries_the_decoded_thumbnail_through() {
        let handle = image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        let toast = screenshot_toast("/tmp/shot.png", Some(handle.clone()));
        assert_eq!(toast.image, Some(handle));
    }

    #[test]
    fn recording_toast_titles_and_names_the_file_with_no_thumbnail() {
        let toast = recording_toast("/home/jordan/Videos/Recording_2026-08-28.webm");
        assert_eq!(toast.summary, "Recording saved");
        assert_eq!(toast.body, "Recording_2026-08-28.webm");
        assert_eq!(
            toast.image, None,
            "a recording is a video file, never something this crate can decode a thumbnail from"
        );
    }

    #[test]
    fn error_toast_carries_the_message_verbatim() {
        let toast = error_toast("ffmpeg was killed by a signal");
        assert_eq!(toast.summary, "Capture error");
        assert_eq!(toast.body, "ffmpeg was killed by a signal");
        assert_eq!(toast.image, None);
    }

    #[test]
    fn a_bare_filename_with_no_directory_is_its_own_body() {
        assert_eq!(
            file_name_or_full_path("just-a-name.webm"),
            "just-a-name.webm"
        );
    }

    #[test]
    fn a_path_with_no_file_name_falls_back_to_the_whole_string() {
        assert_eq!(file_name_or_full_path("/"), "/");
        assert_eq!(file_name_or_full_path(""), "");
    }

    // ------------------------------------------------------------------
    // decode_capture_signal — the real parse, no bus required.
    // ------------------------------------------------------------------

    fn capture_signal<B>(member: &str, body: &B) -> zbus::Message
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        zbus::Message::signal(CAPTURE_OBJECT_PATH, CAPTURE_INTERFACE, member)
            .expect("valid signal coordinates")
            .build(body)
            .expect("serializable body")
    }

    #[test]
    fn capture_taken_decodes_into_a_toast_message_with_no_image_for_a_missing_file() {
        let message = capture_signal("CaptureTaken", &("/does/not/exist.png", "fullscreen"));
        let event = decode_capture_signal(&message, 36.0).expect("well-formed body");
        match event {
            Message::CaptureTaken { path, image } => {
                assert_eq!(path, "/does/not/exist.png");
                assert_eq!(
                    image, None,
                    "a nonexistent file decodes to None, never an error"
                );
            }
            other => panic!("expected CaptureTaken, got {other:?}"),
        }
    }

    #[test]
    fn recording_started_decodes_and_keeps_the_kind() {
        let message = capture_signal("RecordingStarted", &("region",));
        let event = decode_capture_signal(&message, 36.0).expect("well-formed body");
        assert_eq!(
            event,
            Message::RecordingStarted {
                kind: "region".to_string()
            }
        );
    }

    #[test]
    fn recording_finished_decodes_the_path() {
        let message = capture_signal("RecordingFinished", &("/tmp/rec.webm",));
        let event = decode_capture_signal(&message, 36.0).expect("well-formed body");
        assert_eq!(
            event,
            Message::RecordingFinished {
                path: "/tmp/rec.webm".to_string()
            }
        );
    }

    #[test]
    fn error_decodes_the_message() {
        let message = capture_signal("Error", &("disk full",));
        let event = decode_capture_signal(&message, 36.0).expect("well-formed body");
        assert_eq!(
            event,
            Message::Error {
                message: "disk full".to_string()
            }
        );
    }

    #[test]
    fn an_unknown_member_decodes_to_none() {
        let message = capture_signal("SomethingThisBuildPredates", &("whatever",));
        assert_eq!(decode_capture_signal(&message, 36.0), None);
    }

    #[test]
    fn a_mistyped_body_decodes_to_none_rather_than_panicking() {
        // A hand-typed `busctl emit` with the wrong signature — a number
        // where `CaptureTaken`'s two strings belong.
        let message = capture_signal("CaptureTaken", &(7u32,));
        assert_eq!(decode_capture_signal(&message, 36.0), None);
    }

    // ------------------------------------------------------------------
    // decode_owner_change
    // ------------------------------------------------------------------

    fn owner_changed_signal(name: &str, old_owner: &str, new_owner: &str) -> zbus::Message {
        zbus::Message::signal(
            DBUS_DAEMON_OBJECT_PATH,
            DBUS_DAEMON_INTERFACE,
            NAME_OWNER_CHANGED_MEMBER,
        )
        .expect("valid signal coordinates")
        .build(&(name, old_owner, new_owner))
        .expect("serializable body")
    }

    #[test]
    fn capture_losing_its_name_to_nobody_is_a_vanish() {
        let message = owner_changed_signal(CAPTURE_SERVICE_NAME, ":1.42", "");
        assert_eq!(
            decode_owner_change(&message),
            Some(Message::CaptureVanished)
        );
    }

    #[test]
    fn capture_losing_its_name_to_a_fresh_instance_is_still_a_vanish() {
        let message = owner_changed_signal(CAPTURE_SERVICE_NAME, ":1.42", ":1.99");
        assert_eq!(
            decode_owner_change(&message),
            Some(Message::CaptureVanished)
        );
    }

    #[test]
    fn a_fresh_claim_from_nothing_is_not_a_vanish() {
        let message = owner_changed_signal(CAPTURE_SERVICE_NAME, "", ":1.42");
        assert_eq!(decode_owner_change(&message), None);
    }

    #[test]
    fn no_actual_change_is_not_a_vanish() {
        let message = owner_changed_signal(CAPTURE_SERVICE_NAME, ":1.42", ":1.42");
        assert_eq!(decode_owner_change(&message), None);
    }
}
