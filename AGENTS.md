# AGENTS.md — saola-notifications

Notification system for Saola, a Linux desktop environment built in Rust
(iced 0.14 + zbus) targeting the **niri** Wayland compositor. One binary,
three surfaces of responsibility: the freedesktop notification **daemon**
(`org.freedesktop.Notifications`), the **toast** popup stack, and the
**notification centre**. Every sibling repo reserves this slot explicitly —
saola-capture ships an interim toast renderer waiting to be handed over,
and saola-theme already carries the notification card styles, motion
helpers, and tokens.

**Keep this file current.** Every PLAN.md stage that changes commands,
architecture, dependencies, or conventions updates this file in the same
stage and says so in its handoff. A stale AGENTS.md is a bug.

> Status: **Stage 9 done — the control interface finished.**
> `src/modules/centre.rs` is style guide §6's centre: 460 px
> (`sizes.notification_centre_width`), anchored 72 px from the top and 26 px
> from the right, history grouped by application into collapsible groups
> (the theme's own `widget::group_header`, count chip included), a
> do-not-disturb toggle reflecting manual DND, per-row dismiss and a
> clear-all row. A row is Stage 6's `toast::card_view` at `alpha = 1.0,
> life = None`. Dismissals emit `NotificationClosed(id, 2)`
> (`Store::dismiss_notification`, `Store::clear_all`).
>
> The surface **hugs its content**: `centre_height` is a pure function of
> the grouped model, clamped to §6's `100% - 98px`. Because
> `iced_layershell` 0.19 exposes no output geometry, the daemon **measures**
> that clamp once per process — the first open spawns the centre anchored
> top *and* bottom at zero height (the layer-shell protocol's own "stretch
> me"), reads the configured height off the `Opened` event, and respawns at
> the hug height. See `main.rs`'s `CentreMode` / `CentreClamp`. Resizing is
> unmap-then-respawn, keyed on the mode, and recomputed only at
> open/model-change boundaries.
>
> Everything Stages 5 and 6 shipped is unchanged apart from one fix:
> `Daemon::on_notify` now resyncs **both** surfaces, because a `Notify`
> always lands in history and therefore always changes an open centre's
> height.
>
> **Stage 8 done — the capture bridge and auto-DND.**
> `src/modules/capture_bridge.rs` consumes the four frozen `io.saola.Capture1`
> signals via a bare `zbus::MatchRule` (no proxy) plus a second `MatchRule`
> on `org.freedesktop.DBus`'s `NameOwnerChanged`, both merged in one
> `tokio::select!` loop on a second `Connection::session()` (Stage 5's own
> D-Bus worker owns the first; the two are independent `iced::Subscription`s
> with no shared connection). `CaptureTaken`/`RecordingFinished`/`Error` push
> a native toast (`app_name = "saola-capture"`) through the same
> `Daemon::push_notification` path a bus `Notify` uses, with a real id from
> `Daemon::capture_ids` — a *separate* range (`0x8000_0000` upward) from
> `dbus.rs`'s bus-facing `IdAllocator`, reserved by construction rather than
> shared, so `NotificationClosed` for these is a real id like any other.
> Auto-DND's own state (`Daemon::recording`,
> `modules::capture_bridge::RecordingState`) turns on for
> `RecordingStarted` and off for **either** `RecordingFinished` **or**
> `Error` — a live finding, not PLAN.md's literal words: reading
> `saola-capture::dbus`'s own recording-finalization code shows a failed
> recording emits `Error` *instead of* `RecordingFinished`, never both, so
> treating only `RecordingFinished` as the "off" trigger would leak
> auto-DND on forever after any recording failure. `Daemon::set_recording`
> is the one place `recording_dnd` is written and logged. The
> `NameOwnerChanged` leak guard clears it if capture vanishes mid-recording
> — confirmed live by actually killing the capture process, not only by
> unit test. `store.rs::decode_path_str` (now `pub(crate)`) decodes
> `CaptureTaken`'s thumbnail; `image`'s `webp` feature was added this stage
> because saola-capture's *default* `image-format` is `webp`, not `png` —
> found live, a real gap in the naive "png decoder only" reading of PLAN.md's
> own task prose.
>
> **Stage 9 done — the control interface finished.** All four
> `io.saola.Notifications1` properties are live and emit `PropertiesChanged`.
> `dbus::ControlState` (four plain fields, `NotificationCount`/`DndActive`/
> `DndManual`/`CentreOpen`) sits behind an `Arc<Mutex<_>>` —
> `dbus::SharedControlState` — created in `dbus::serve` and handed back to
> `main.rs` on `Message::BusReady`; `ControlService`'s property getters only
> ever read it, `Daemon::sync_control_state` is the only writer.
> `dbus::ControlState::changed` is the pure diff (unit-tested) that decides
> which properties actually changed since the last write; `dbus::
> sync_control_state` writes the new snapshot then calls the zbus-generated
> `<property>_changed(emitter)` once per changed property, via
> `connection.object_server().interface::<_, ControlService>(path)` (the
> same `InterfaceRef` pattern zbus's own `issue_310` regression test uses —
> `main.rs`'s `update` cannot hold a `SignalEmitter` the way a served method
> does). `NotificationCount` is `Store::history().len()` — the same list the
> centre shows, not the live toast-stack count. `sync_control_state` is
> called from every `update` arm that can change one of the four
> properties, and is a cheap no-op before `BusReady`. `DismissAll` and
> `Dismiss` were changed to call `Store::clear_all`/`dismiss_notification`
> (both toast stack *and* history) rather than `dismiss_toast`/
> `dismiss_all_toasts` (toast-only) — the latter is now gone,
> the former's `Message::Dismiss` scope is a Stage 9 judgment call
> (see the Stage 9 handoff). `io.saola.Notifications1` is documented as a
> FROZEN contract in README.md's Architecture section — method and property
> semantics live there now, not only in PLAN.md.
>
> PLAN.md is the staged build plan
> and its **Context**, **Architecture**, and **Frozen external contracts**
> sections are binding; read them before any implementation work. This file
> summarizes the rules
> that must hold in every stage.

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings   # CI gate — warnings are errors
cargo fmt --check                           # CI gate
cargo run                                   # needs BOTH a Wayland session (niri) and a D-Bus session bus
```

Live-testing anything that maps surfaces happens in a **nested niri**
(`niri` in a window), never Jordan's real session, until the surface is
proven to grab no keyboard. The recipe Stage 5 settled on:

```sh
niri &                                   # nested; its socket appears in its own log
export WAYLAND_DISPLAY=wayland-2         # whatever that log announced
export NIRI_SOCKET=/run/user/1000/niri.wayland-2.<pid>.sock   # for `niri msg`
dbus-run-session -- bash -c '…'          # mako owns org.freedesktop.Notifications on
                                         # the real bus, so the daemon AND notify-send
                                         # must share one disposable bus
niri msg layers                          # the only introspection that lists layer surfaces
grim shot.png                            # composited output of the nested compositor
```

**Known caveat (Stage 5, niri 26.04):** inside a nested (winit-backed) niri,
an `iced_layershell` `StartMode::Background` daemon's on-demand surfaces are
created, positioned and destroyed correctly — `niri msg layers` proves it —
but never paint a visible frame. Reproduced identically with saola-capture's
own known-good toast, so it is a property of the nested compositor, not of
this crate. Verify surface **lifecycle** with `niri msg layers` there; visual
confirmation needs a real niri session.

## Architecture (binding — PLAN.md's Architecture section is the source)

- **One binary, one process**: `iced_layershell::build_pattern::daemon`
  booting with ZERO surfaces, spawning them on demand (model on
  saola-capture's `src/main.rs`). One tokio runtime shared by iced and
  zbus (`zbus` with `default-features = false, features = ["tokio"]`) —
  never two runtimes.
- **Two surface roles**, both `Layer::Overlay`, exclusive zone 0, anchored
  Top|Right:
  - `Toasts`: exists only while the stack is non-empty,
    `KeyboardInteractivity::None`, respawn-to-resize on count change
    (capture's `sync_toast_surface` pattern).
  - `Centre`: exists only while open, `KeyboardInteractivity::OnDemand`,
    closes on Escape and focus loss, height hugs content via a pure
    `centre_height(theme, &model)` clamped to `output_height − 98`.
    That clamp is **measured, never assumed** (Stage 7): `iced_layershell`
    0.19 exposes no output geometry, so the first open of each process
    spawns the centre in `CentreMode::Measure` — anchored Top|Bottom|Right,
    zero height, input-transparent, painting nothing — which the layer-shell
    protocol requires the compositor to stretch, and the `Opened` event's
    height *is* `output_height − 98`. It then respawns in `CentreMode::Hug`.
    Never invent a fallback screen height; `CentreClamp::Unavailable` (an
    unclamped, possibly overhanging centre) is the documented answer when a
    compositor will not stretch.
  - **Every model change that touches history must resync the centre**, not
    just the toast stack — a `Notify` lands in history even when DND
    suppresses its toast. `Daemon::on_notify` batches both syncs.
- **D-Bus bridge** (capture's `dbus.rs` shape): served interfaces hold an
  `iced::futures::channel::mpsc::Sender<DaemonEvent>`; a
  `Subscription::run` worker connects, registers the object server, claims
  names, and relays events into `Message`. `try_send` from served methods,
  never `.send().await`. `Notify` answers synchronously, so id allocation
  lives in the service (`AtomicU32`, start 1, skip 0 on wrap) — the UI
  never answers the bus. Signals are emitted from `update` via
  `Task::future` + the interface's `SignalEmitter` once `BusReady`
  delivers the connection.
- **Name claims**: `RequestNameFlags::DoNotQueue` alone, never
  `ReplaceExisting`. `org.freedesktop.Notifications` taken (mako/dunst
  running) → log and keep running; `io.saola.Notifications1` taken →
  second instance → exit 0.
- **Frozen external contracts** (PLAN.md section of the same name):
  `org.freedesktop.Notifications` at `/org/freedesktop/Notifications`,
  `io.saola.Notifications1` at `/io/saola/Notifications1` (the saola-panel
  indicator's contract — all properties emit `PropertiesChanged`), and the
  four consumed `io.saola.Capture1` signals. Never change names or
  semantics without Jordan's sign-off.
- **DND policy**: `effective_dnd = manual || recording`. Critical urgency
  bypasses manual DND (config-gated) but NEVER recording auto-DND — no
  toast is ever burned into a screencast. Suppressed notifications still
  land in history. `recording` turns on for `RecordingStarted` and off for
  **either** `RecordingFinished` or `Error` (a failed recording emits one or
  the other, never both — `modules::capture_bridge`'s module doc comment),
  plus a `NameOwnerChanged` leak guard if `saola-capture` vanishes off the
  bus while it was on.

## Module pattern (binding)

One file per module under `src/modules/`, each exposing a state struct +
`pub enum Message` + `fn view(&self, theme: &Theme) -> Element<'_, Message>`
+ `fn subscription(&self) -> Subscription<Message>`; the outer `Message`
nests each module's enum. The pattern is documented in saola-panel's
`src/modules/mod.rs` — copy its doc comment, don't invent.

## Resilience rules (binding)

- No panics — no `panic!`/`unwrap`/`expect` on any runtime path.
- An absent service renders nothing rather than killing the process.
- Every module maps to a signal, never a poll; nothing ticks without a
  documented exception.
- Time is always injected (`Instant` parameters), never read inside the
  store — this is what keeps expiry and DND logic unit-testable.

## Design language and the theme-gap protocol (binding)

- **Zero hardcoded colors, sizes, or durations.** Every value comes from
  `saola-theme` — a git dependency **pinned to a release tag** (`tag` and
  `version` move together, never `branch = "main"`).
- The UI spec is `docs/SAOLA-STYLE-GUIDE.md` §5 (toast timing — exact) and
  §6 (notification card, notification centre). The theme's motion tokens
  encode the timing.
- **Missing style or token?** Follow the theme-gap protocol, in order:
  1. Record the gap in `docs/UPSTREAM-THEME-DEBT.md` — the file is the
     contract.
  2. Notify Jordan's open `saola-theme` session via SendMessage (find it
     with ListAgents).
  3. Use the closest existing helper locally in the meantime.
  Verify any "done" claim from that session against the theme repo and its
  release tag before bumping the pin.

## Conventions

- **Teaching notes**: Jordan is newer to Rust — comment the non-obvious
  (async ownership, proxy macros, stream bridging, the layershell respawn
  dance); prefer explicit code over clever abstraction.
- **Config**: `notifications.toml`, hand-walked over `toml::Table` — never
  `#[derive(Deserialize)]`. One bad knob degrades alone with its own
  precise warning; a malformed or absent file yields full defaults, never
  a crash. Resolution chain: `$SAOLA_CONFIG_DIR` → `$XDG_CONFIG_HOME/saola`
  → `~/.config/saola`; empty env vars count as unset.
- **Dependencies**: every non-trivial addition carries a dated survey
  comment in Cargo.toml (alternatives considered, why they lost — read
  capture's Cargo.toml for the voice).
- **Testing**: pure logic unit-tested inline (`#[cfg(test)] mod tests`);
  D-Bus and surface behavior is manual evidence recorded in stage
  handoffs. Never `std::env::set_var` in a test.
- **Releases**: Conventional Commits; release-plz generates the version
  and CHANGELOG.md — never hand-edit either. Tags are
  `saola-notifications-vX.Y.Z`. User-facing prose (README, commit
  subjects) is ASD-STE100 Simplified Technical English.

## Boundaries (binding)

- v0.1 history is in-memory only — no persistence.
- Post-v0.1 roadmap, do NOT build now: per-app config rules,
  `saola-notifyctl` CLI, inline reply, media footer, sounds.
- Never run `sudo`; never edit Jordan's niri or user config — print the
  commands and wait.
