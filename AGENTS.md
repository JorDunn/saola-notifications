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

> Status: Stage 4 done — `notifications.toml` loads at boot
> (`src/config.rs`, hand-walked `toml::Table`, never `serde::Deserialize`)
> and live-reloads over inotify (`src/config_watch.rs`, wired but inert
> until Stage 5's daemon calls it from its own `subscription()`). The D-Bus
> bridge (`src/dbus.rs`) serves both frozen interfaces headlessly:
> `NotificationsService` (`org.freedesktop.Notifications`) and
> `ControlService` (`io.saola.Notifications1`), each forwarding a
> `DaemonEvent` over an `iced::futures::channel::mpsc` channel that
> `main.rs`'s plain `#[tokio::main]` runner drains and logs. `src/store.rs`
> is now the pure notification model behind that bridge — hint parsing
> (urgency, transient, resident, the six-alias image lookup and `iiibiiay`
> decode), Pango/HTML body-markup stripping, DND policy, expiry policy (a
> pausable stopwatch), and the in-memory toast-stack/history `Store` with
> its replace-vs-same-app rules — but nothing calls into it yet (wired
> `#[allow(dead_code)]`, same as `config_watch.rs`). No UI surface exists
> yet (Stage 5). PLAN.md is the staged build plan and its **Context**,
> **Architecture**, and **Frozen external contracts** sections are binding;
> read them before any implementation work. This file summarizes the rules
> that must hold in every stage.

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings   # CI gate — warnings are errors
cargo fmt --check                           # CI gate
cargo run                                   # needs a D-Bus session bus; Wayland (niri) arrives Stage 5
```

Live-testing anything that maps surfaces happens in a **nested niri**
(`niri` in a window), never Jordan's real session, until the surface is
proven to grab no keyboard.

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
  land in history.

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
