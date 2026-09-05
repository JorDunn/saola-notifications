---
project_type: rust
handoff_dir: .claude/handoffs
max_retries: 1
on_failure: escalate
---

# saola-notifications — v0.1 build plan

## Context

Saola is a Rust/iced Linux desktop environment targeting the niri
compositor. `saola-notifications` is its notification system: the
freedesktop notification daemon, toast popups, and notification centre.
Every sibling repo reserves this slot explicitly — saola-panel rejects a
`notifications` module by name, saola-capture ships an interim toast
renderer with a `toasts` kill-switch waiting to be handed over, and
saola-theme already ships the notification card styles, motion helpers,
and tokens.

v0.1 ships all three surfaces: daemon + toasts + centre. History is
in-memory only. Manual do-not-disturb is part of the centre; auto-DND
engages while saola-capture records. Critical urgency bypasses manual
DND but NOT recording auto-DND (never burn a toast into a screencast; it
still lands in history). Post-v0.1 roadmap (do not build now): per-app
config rules, `saola-notifyctl` CLI, inline reply, media footer, sounds.

## Architecture (binding — every stage conforms to this)

- **One binary, one process**: `iced_layershell::build_pattern::daemon`
  booting with ZERO surfaces, spawning them on demand. Model on
  `/home/jordan/Developer/saola-capture/src/main.rs`. Single tokio
  runtime shared by iced and zbus (`zbus = { version = "5",
  default-features = false, features = ["tokio"] }`) — never two
  runtimes.
- **Two surface roles**, both `Layer::Overlay`, exclusive zone 0,
  anchored Top|Right:
  - `Toasts`: `notification_card_width` × stack height, margins
    (top 72 = `popover_top`, right 26), `KeyboardInteractivity::None`,
    exists only while the stack is non-empty. Respawn-to-resize on count
    change (capture's `sync_toast_surface` pattern).
  - `Centre`: `notification_centre_width` × content height clamped to
    `output_height − 98`, `KeyboardInteractivity::OnDemand`, exists only
    while open; closes on Escape and focus loss. Height hugs content via
    a pure `centre_height(theme, &model)` from token-derived row
    heights; inner list scrolls past the clamp.
- **D-Bus bridge** (capture `dbus.rs` shape): served interfaces hold an
  `iced::futures::channel::mpsc::Sender<DaemonEvent>`; a
  `Subscription::run` worker connects, registers the object server,
  claims names, relays events into `Message`. `try_send` from served
  methods, never `.send().await`. `Notify` returns its id synchronously,
  so id allocation lives in the service (`AtomicU32`, start 1, skip 0 on
  wrap) — the UI never answers the bus. Once serving, the worker sends
  `Message::BusReady(zbus::Connection)`; `update` stores the connection
  and emits `NotificationClosed`/`ActionInvoked`/`PropertiesChanged` via
  `Task::future` + the interface's `SignalEmitter`.
- **Name claims** — `RequestNameFlags::DoNotQueue` alone, never
  `ReplaceExisting` (rule from
  `/home/jordan/Developer/saola-session/src/modules/inhibit.rs`):
  `org.freedesktop.Notifications` taken (mako/dunst running) → log and
  keep running (control interface + capture toasts + centre still work);
  `io.saola.Notifications1` taken → second instance → exit 0.
- **Module pattern** (binding, documented in
  `/home/jordan/Developer/saola-panel/src/modules/mod.rs`): one file per
  module exposing a state struct + `pub enum Message` +
  `fn view(&self, theme: &Theme) -> Element<'_, Message>` +
  `fn subscription(&self) -> Subscription<Message>`; outer `Message`
  nests each module's enum.
- **Resilience rules**: no panics; an absent service renders nothing
  rather than killing the process; every module maps to a signal, never
  a poll; time is always injected, never read inside the store.
- **Zero hardcoded colors/sizes.** Styles come from `saola-theme`
  (git tag pin, `tag` and `version` move together, never
  `branch = "main"`). If a style or token is missing: (1) record the gap
  in `docs/UPSTREAM-THEME-DEBT.md` — the file is the contract; (2)
  notify Jordan's open `saola-theme` session via SendMessage (find it
  with ListAgents) announcing the need; (3) use the closest existing
  helper locally in the meantime. Verify any "done" claim from that
  session against the theme repo and its release tag before bumping the
  pin.
- **UI spec** is `docs/SAOLA-STYLE-GUIDE.md` §5 (toast timing — exact)
  and §6 (notification card, notification centre). The theme's motion
  tokens encode the timing; never hardcode durations.
- **Teaching-note comments**: Jordan is newer to Rust — comment the
  non-obvious (async ownership, proxy macros, stream bridging, layershell
  respawn dance); prefer explicit code over clever abstraction.

## Frozen external contracts

- Serve `org.freedesktop.Notifications` at
  `/org/freedesktop/Notifications`: `Notify`, `CloseNotification`,
  `GetCapabilities`, `GetServerInformation`, signals
  `NotificationClosed(id, reason)` and `ActionInvoked(id, key)`.
  `GetServerInformation` = ("saola-notifications", "Saola",
  CARGO_PKG_VERSION, "1.2"). `GetCapabilities` v0.1 = `body`, `actions`,
  `icon-static`, `persistence` (no `body-markup`, `sound`,
  `action-icons`).
- Serve `io.saola.Notifications1` at `/io/saola/Notifications1`:
  methods `ToggleCentre()`, `OpenCentre()`, `CloseCentre()`,
  `SetDnd(b)` (manual only), `DismissAll()`, `Dismiss(u id)`;
  properties (all emit `PropertiesChanged` — this is the saola-panel
  indicator's contract) `NotificationCount: u`, `DndActive: b`
  (effective), `DndManual: b`, `CentreOpen: b`. No custom signals.
- Consume `io.saola.Capture1` signals (frozen by saola-capture):
  `CaptureTaken(path, kind)`, `RecordingStarted(kind)`,
  `RecordingFinished(path)`, `Error(message)`.

## Stage 1 — Repo skeleton and dependency survey

```yaml
model: sonnet
effort: medium
tools: [Read, Grep, Glob, Write, Edit, Bash]
verify:
  files: [Cargo.toml, src/main.rs, LICENSE-MIT, LICENSE-APACHE, README.md, CHANGELOG.md, rustfmt.toml, rust-toolchain.toml, release-plz.toml, .github/workflows/ci.yml, contrib/systemd/saola-notifications.service, docs/SAOLA-STYLE-GUIDE.md, docs/UPSTREAM-THEME-DEBT.md]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build"
```

Read this PLAN.md's Context and Architecture sections first. Work in
/home/jordan/Developer/saola-notifications (currently: one-line
README.md, a single LICENSE file, a Rust .gitignore).

Build the family-standard repo skeleton by cloning conventions from the
newest siblings, /home/jordan/Developer/saola-files and
/home/jordan/Developer/saola-capture (read them — do not invent):

1. **Licensing**: delete the single `LICENSE`; copy `LICENSE-MIT` and
   `LICENSE-APACHE` from saola-capture. `license = "MIT OR Apache-2.0"`
   in Cargo.toml.
2. **Docs pair**: `AGENTS.md` carrying the repo instructions and a
   one-line `CLAUDE.md` containing `@AGENTS.md` (the capture/files
   convention). AGENTS.md summarizes this PLAN.md's Architecture rules,
   the module pattern, resilience rules, and the theme-gap protocol.
3. **README.md**: family template — H1, Status, Building, Running,
   Configuring, Schema (annotated config sample; can be minimal until
   Stage 2), Architecture for contributors, License. Prose in
   ASD-STE100 Simplified Technical English.
4. **CHANGELOG.md**: Keep a Changelog 1.1.0 header, empty Unreleased
   section (release-plz generates entries).
5. **Tooling configs**: copy `rustfmt.toml`, `rust-toolchain.toml`,
   `release-plz.toml` (publish = false,
   git_tag_name = "{{ package }}-v{{ version }}",
   features_always_increment_minor = true), `.github/workflows/ci.yml`
   (jobs fmt / clippy -D warnings / test / build), `release-plz.yml`,
   `pkgbuild-release.yml` from saola-files, adapting names.
6. **contrib/**: `contrib/aur/PKGBUILD` template (@PKGVER@ + SKIP sha,
   from a sibling), `contrib/systemd/saola-notifications.service` user
   unit (`After=graphical-session.target`,
   `ConditionEnvironment=XDG_CURRENT_DESKTOP=niri`,
   `Restart=on-failure`, `RestartSec=2s`, `StartLimitIntervalSec=0`).
7. **docs/**: vendor `docs/SAOLA-STYLE-GUIDE.md` byte-identical from
   /home/jordan/Developer/saola-theme/design/SAOLA-STYLE-GUIDE.md; seed
   `docs/UPSTREAM-THEME-DEBT.md` (empty table, format from
   /home/jordan/Developer/saola-files/docs/UPSTREAM-THEME-DEBT.md).
8. **Cargo.toml**: edition 2024, binary crate `saola-notifications`.
   Every dependency carries a dated survey comment justifying the pick
   and exact feature set (family convention — read capture's Cargo.toml
   for the voice). Dependencies: `iced 0.14` (default-features = false,
   features `tokio,svg,advanced,image-without-codecs`; survey whether
   the `image` crate is additionally needed for png decode of
   image-path/capture thumbnails), `iced_layershell 0.19`,
   `saola-theme = { git = "https://github.com/JorDunn/saola-theme",
   tag = "saola-theme-v0.13.0", version = "0.13.0" }`, `zbus 5`
   (default-features = false, features tokio), `toml`, `tracing`,
   `tracing-subscriber`, `futures` (match sibling versions).
9. **src/main.rs**: a stub that initializes tracing and exits cleanly —
   just enough for the verify command to pass. No layershell yet.

Done means: verify command green locally; a handoff note listing every
file created and any convention deviations.

## Stage 2 — Config and live reload

```yaml
model: sonnet
effort: medium
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [1]
verify:
  files: [src/config.rs, src/config_watch.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture section and the Stage 1 handoff first.

Implement `notifications.toml` support, test-first:

- **Resolution chain** (identical across the family — read
  /home/jordan/Developer/saola-capture/src/config.rs): `$SAOLA_CONFIG_DIR`
  → `$XDG_CONFIG_HOME/saola` → `~/.config/saola`; empty string counts as
  unset; paths always absolutized. File: `notifications.toml`.
- **Hand-walked parsing** over `toml::Table` — never
  `serde::Deserialize` derive. One bad knob degrades alone with its own
  precise `tracing` warning; a malformed or absent file yields full
  defaults, never a crash or panic.
- **Keys** (v0.1): `dnd-default` (bool, false), `history-cap`
  (int, 100), `critical-bypasses-dnd` (bool, true). Reserve a
  commented-out `[apps]` table in the sample schema for the post-v0.1
  per-app rules — parse nothing from it yet.
- **Live reload**: copy
  /home/jordan/Developer/saola-panel/src/config_watch.rs (inotify with
  the rename/inode caveat it documents), adapt the filename.
- Update README's Schema section with the annotated sample config
  (ASD-STE100 prose).

Unit tests cover: default fallback, each key parsing, one-bad-knob
degradation, resolution-chain precedence (use temp dirs + env vars, no
writes outside temp).

## Stage 3 — D-Bus skeleton (no UI)

```yaml
model: sonnet
effort: high
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [2]
verify:
  files: [src/dbus.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture and Frozen external contracts sections
and prior handoffs first.

Create `src/dbus.rs` serving both interfaces headlessly (main.rs is
still a plain tokio runner at this stage — layershell arrives in
Stage 5):

- Model the file on /home/jordan/Developer/saola-capture/src/dbus.rs
  (serve + proxy in one file, `DaemonEvent` enum, channel sender held by
  the services). One `#[zbus::interface]` per handler struct:
  `NotificationsService` (org.freedesktop.Notifications) and
  `ControlService` (io.saola.Notifications1) — the saola-files
  two-interface rule.
- Conditional name claims exactly per the Architecture section's
  posture. Read /home/jordan/Developer/saola-session/src/modules/inhibit.rs
  for the `RequestNameFlags::DoNotQueue` shape and teaching notes.
- `Notify` parses nothing deep yet: allocate the id (AtomicU32 semantics
  from Architecture), honor `replaces_id` by echoing it, log the call,
  forward a `DaemonEvent`, return the id. `CloseNotification` emits
  `NotificationClosed(id, 3)`. `GetCapabilities` and
  `GetServerInformation` return the frozen values. Control methods
  forward `DaemonEvent`s; properties return placeholder state for now.
- Unit-test what is pure (id allocation wrap/skip-0, capability list);
  integration behavior is manual evidence.

Done means (record in handoff with command output): with the daemon
running under a session bus, `notify-send hello world` returns and
`busctl --user call` on `Notify` yields an id;
`busctl --user introspect` shows both interfaces at their paths; when
mako or dunst already owns org.freedesktop.Notifications the daemon
logs and stays alive, and a second saola-notifications instance exits 0.

## Stage 4 — Store and hint parsing (pure core)

```yaml
model: sonnet
effort: high
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [3]
verify:
  files: [src/store.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture section and prior handoffs first.

Build `src/store.rs`: the pure notification model. Hard constraints —
no zbus imports, no clock reads (time is injected as `Instant`
parameters), no iced widget code (the `image::Handle` type for decoded
images is the one allowed iced type). Everything unit-tested.

- `Notification { id: u32, app_name, app_icon, summary, body, actions:
  Vec<Action { key, label }>, urgency: Low|Normal|Critical, image:
  Option<iced::widget::image::Handle>, expire_timeout, transient: bool,
  resident: bool, posted_at: Instant }`.
- **Hint parsing** from the raw `Notify` argument types: urgency (byte
  0/1/2), `transient`, `resident`, image lookup order `image-data` →
  `image_data` → `image-path` → `image_path` → `app_icon` →
  legacy `icon_data` (the spec's alias mess — handle all). `image-data`
  is the `iiibiiay` struct: decode, convert rowstride/channels to RGBA,
  downsample to the theme's `icon_tile` size (nearest-neighbour is
  fine — see capture's thumbnail precedent),
  `Handle::from_rgba` only (sync decode rule). Decode failure → `None`
  (themed fallback tile is the UI's job), never an error.
- **Body markup**: strip Pango/HTML tags at parse time (clients send
  them regardless of capabilities). Keep it dependency-light — a small
  hand-rolled stripper with tests beats pulling a parser crate.
- **Replace semantics**: `replaces_id != 0` replaces that record
  everywhere (toast resets its stopwatch, history entry replaced
  in place). Separately, style-guide §6: a second notification from an
  app already on-screen replaces its toast card and resets the clock but
  appends a NEW history entry. Both rules, both tested.
- **Expiry policy** (pausable stopwatch, frozen-total + resumed_at —
  lift the shape from /home/jordan/Developer/saola-capture/src/modules/toast.rs):
  `expire_timeout` −1 → theme default (motion.toast_idle); >0 →
  replaces the idle span; 0 or urgency Critical → no life rule, never
  auto-dismiss. Close reasons: 1 expired, 2 user-dismissed, 3
  CloseNotification.
- **DND**: `effective_dnd = manual || recording`. Suppressed
  notifications skip the toast queue but land in history. Critical
  bypasses manual DND only (config `critical-bypasses-dnd`), never
  recording DND. Table-driven tests over
  (urgency × manual × recording × config).
- **History**: in-memory `Vec`, capped at config `history-cap`
  (oldest dropped), grouped by `app_name` at view time,
  `collapsed: HashSet<String>`. Toast stack max = theme
  `toast_max_stack` (3): fourth replaces oldest.

Test list (minimum): id-wrap skip-0, every hint alias, `iiibiiay`
decode + rowstride conversion, markup strip cases, the expiry policy
table, the DND table, replace-vs-same-app rules, history cap.

## Stage 5 — iced daemon and toast surface (visible MVP)

```yaml
model: opus
effort: high
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [4]
verify:
  files: [src/main.rs, src/modules/mod.rs, src/modules/toast.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture section, style guide §5 and §6
(docs/SAOLA-STYLE-GUIDE.md), and prior handoffs first. This is the
riskiest UI stage — opus for a reason. Key reference files:

- /home/jordan/Developer/saola-capture/src/main.rs — the
  daemon-with-zero-boot-surfaces shape, `sync_toast_surface`,
  `toast_surface_settings`, `#[to_layer_message(multi)]`.
- /home/jordan/Developer/saola-capture/src/modules/toast.rs — the
  interim toast implementation explicitly written to be lifted here:
  hover-pause stopwatch, slide-in via shrinking leading spacer (iced
  0.14 has no subtree transform), fade via per-color alpha scaling (no
  subtree opacity), injected time.
- saola-theme v0.13 helpers to consume instead of local styles:
  `style::container::notification_card(t, alpha)`, `card_urgent`,
  `style::notification::{life_rule, icon_tile}`,
  `motion::{toast_alpha, life_fraction}`, tokens
  `notification_card_width`, `popover_top`, `motion.toast_*`,
  `toast_max_stack`.

Work:

1. Convert `main.rs` to the layershell daemon: outer `Message` with
   `#[to_layer_message(multi)]`, `SurfaceRole { Toasts, Centre }`
   registry (Centre is a stub arm until Stage 7), the D-Bus
   subscription from Stage 3 bridged in, `BusReady` connection storage,
   signal emission via `Task::future` per the Architecture section.
2. Lift and generalize toast.rs into `src/modules/toast.rs`: back it
   with Stage 4's `Notification` instead of capture's `ToastKind`; icon
   tile shows the decoded image or themed fallback; urgent cards use
   `card_urgent`, have no life rule, and never auto-dismiss; normal
   cards follow §5 exactly (0.35s slide-in from right with fade, 5s
   rest, 1s fade in place, life rule 1→0, hover pauses both).
3. Surface lifecycle per Architecture: map on first card, respawn on
   count change, unmap on last expiry. Expiry and click-dismiss emit
   `NotificationClosed` with reasons 1 and 2.
4. `src/modules/mod.rs` carries the module-pattern doc comment (adapt
   from saola-panel's).
5. Any missing theme style (likely: the 26px right margin token, ivory
   action-pill-on-ink) → follow the theme-gap protocol in Architecture
   (UPSTREAM-THEME-DEBT.md entry + message the `saola-theme` session).

Unit-test everything pure (stack replacement policy, alpha/lifecycle
math against injected times). Manual evidence for the handoff, run
inside a nested niri (`niri` in a window) with the daemon running:
`notify-send Test body` shows a §5-exact toast that expires on
schedule; hover pauses; `notify-send -u critical Urgent` persists
indefinitely with the urgent ring; four rapid notifications keep the
stack at three.

## Stage 6 — Actions

```yaml
model: sonnet
effort: medium
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [5]
verify:
  files: [src/modules/toast.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture section, style guide §6, and prior
handoffs first.

Implement notification actions on the toast card:

- Render action pills (ivory-on-ink, from saola-theme button/pill
  styles — theme-gap protocol if missing) below the body, per §6.
- The `"default"` action key renders no pill; it fires on card click.
  Without a default action, card click dismisses (reason 2) as before.
- Invoking an action emits `ActionInvoked(id, key)` then closes the
  toast (reason 2) unless the notification is `resident`.
- The centre will reuse this rendering later — keep the card view
  reusable (a function over `&Notification`, not toast-state-coupled).

Unit-test the action policy (default vs pills, resident behavior).
Manual evidence: `notify-send -A yes=Yes -A no=No "Pick"` shows two
pills; clicking one produces `ActionInvoked` visible in
`busctl --user monitor`, and the toast closes.

## Stage 7 — Notification centre

```yaml
model: opus
effort: high
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [6]
verify:
  files: [src/modules/centre.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture section, style guide §6 ("Notification
centre"), and prior handoffs first. The centre's resize behavior is the
riskiest mechanic in the project — layershell resizes by respawn, so
recompute height only on open/model-change boundaries; if hug-height
respawn proves unstable, fall back to a full-clamp-height surface and
document the click-swallow cost in a code comment and the handoff.

Build `src/modules/centre.rs` and the Centre surface arm in main.rs:

- 460px (`notification_centre_width`), anchored 72px from top
  (`popover_top`) / 26px from right, max height `output_height − 98`,
  hugs content via pure `centre_height(theme, &model)` (unit-tested),
  inner `scrollable` list past the clamp.
- History grouped by app: collapsible group headers (use saola-theme's
  group-header row widget), per-group count chips, per-item dismiss,
  clear-all row, DND toggle reflecting `DndManual` and toggling it.
- Opens/closes via `ToggleCentre`/`OpenCentre`/`CloseCentre` on the
  control interface; `KeyboardInteractivity::OnDemand`; Escape closes;
  focus loss (Unfocused event) closes; toggling while open closes.
  Never two centre surfaces.
- Dismissals from the centre emit `NotificationClosed(id, 2)`.
- Missing styles (likely: centre container, empty-state, clear-all
  row) → theme-gap protocol.

Unit tests: `centre_height` cases (empty, one group, collapsed groups,
clamp), group/collapse model logic. Manual evidence in nested niri:
open via `busctl --user call ... ToggleCentre`, verify §6 geometry,
collapse/expand, per-item dismiss, clear-all, Escape and focus-loss
close.

## Stage 8 — Capture bridge and auto-DND

```yaml
model: sonnet
effort: high
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [7]
verify:
  files: [src/modules/capture_bridge.rs]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Architecture and Frozen external contracts sections
and prior handoffs first.

Build `src/modules/capture_bridge.rs`: a signal-listener zbus bridge
(no proxy object needed — use `zbus::MatchRule` +
`MessageStream::for_match_rule`; read
/home/jordan/Developer/saola-panel/src/modules/claude.rs for the shape)
consuming the four frozen `io.saola.Capture1` signals:

- `CaptureTaken(path, kind)` → native toast (summary/body/icon per
  capture's own interim toasts — read
  /home/jordan/Developer/saola-capture/src/modules/toast.rs message
  copy); `RecordingFinished(path)` → toast; `Error(message)` → toast.
- `RecordingStarted(kind)` → auto-DND on; `RecordingFinished` →
  auto-DND off. While recording, ALL toasts including critical are
  suppressed (they land in history; the recording-finished toast shows
  after DND lifts).
- **DND leak guard**: also watch `NameOwnerChanged` for the capture
  bus name; if capture vanishes mid-recording, clear auto-DND (the
  session-inhibit peer-vanish precedent). Absent capture daemon =
  bridge renders nothing, no errors.

Unit-test the DND state transitions (started/finished/vanished
orderings, including finish-never-arrives). Manual evidence: with
saola-capture running in nested niri, a screenshot produces a native
toast; starting a recording suppresses `notify-send` toasts (they
appear in the centre) and stopping restores them; killing capture
mid-recording restores toasts too.

## Stage 9 — Control interface finish

```yaml
model: sonnet
effort: medium
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [8]
verify:
  files: [src/dbus.rs, README.md]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Frozen external contracts section and prior
handoffs first.

Finish `io.saola.Notifications1` so the saola-panel indicator can build
against it:

- Wire the real properties — `NotificationCount`, `DndActive`,
  `DndManual`, `CentreOpen` — to live state, each emitting
  `PropertiesChanged` on change (zbus `#[zbus(property)]` +
  the emitter pattern from the Architecture section).
- Implement any control methods still stubbed (`SetDnd`, `Dismiss`,
  `DismissAll`) end-to-end, including `NotificationClosed(_, 2)` for
  dismissals.
- Document the interface in README's Architecture section as a FROZEN
  contract (method/property names and semantics), the way capture
  freezes its signals. ASD-STE100 prose.

Manual evidence: `busctl --user get-property` shows live values;
`busctl --user monitor` shows `PropertiesChanged` firing when a
notification arrives, DND toggles, and the centre opens.

## Stage 10 — Review and release prep

```yaml
model: sonnet
effort: high
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [9]
verify:
  files: [docs/REVIEW-v0.1.md, docs/UPSTREAM-THEME-DEBT.md]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build --release"
```

Read this PLAN.md end to end and all handoffs first.

Close out v0.1 (review first, then prep — do not release; pushing is
Jordan's call):

1. **Silent-failure audit** (the capture Stage-18 / session REVIEW
   shape): sweep every `let _ =`, ignored `Result`, `unwrap`/`expect`,
   and empty match arm; each is either justified with a comment or
   fixed. Verify the resilience rules hold (no panics, absent services
   degrade silently, no polling). Write findings and resolutions to
   `docs/REVIEW-v0.1.md`.
2. **Spec conformance pass**: re-read style guide §5/§6 and the Frozen
   external contracts section; check each number and name against the
   code; list any deviation in the review doc.
3. **UPSTREAM-THEME-DEBT.md**: finalize — every entry has a date, the
   local workaround, and whether the `saola-theme` session was
   notified.
4. **Docs**: README complete (Status, Building, Running, Configuring,
   Schema with full annotated sample, Architecture, frozen-contract
   section, License) — run the ASD-STE100 check from the global
   instructions on README.md and fix findings or add Technical Names to
   `.ste-glossary.yml`.
5. **Packaging**: PKGBUILD template and systemd user unit reviewed
   against the release workflow; `cargo build --release` succeeds.
6. **Handoff**: a summary of v0.1 state, the panel/capture coordination
   items still open (panel indicator module; capture flipping its
   `toasts` default), and the post-v0.1 roadmap order (per-app rules →
   notifyctl → inline reply → media footer → sounds).

## Stage 11 — `HasUrgent` property (v0.2, additive)

```yaml
model: sonnet
effort: medium
skill: test-driven-development
tools: [Read, Grep, Glob, Write, Edit, Bash]
depends_on: [10]
verify:
  files: [src/store.rs, src/dbus.rs, src/main.rs, README.md, contrib/notifications/README.md]
  command: "cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
```

Read this PLAN.md's Frozen external contracts section, AGENTS.md, and
`.claude/handoffs/handoff_stage_10.md` first. Then read
`contrib/notifications/README.md` — it is the canonical contract file
for `io.saola.Notifications1` and already carries `HasUrgent` under a
"Planned (not yet served)" note.

**Why this stage exists.** The saola-panel indicator (the consumer of
`io.saola.Notifications1`) shows a terracotta accent for "something
needs you". Under DND the toast is suppressed, so `NotificationCount`
alone cannot tell the bar that a *critical* notification is waiting.
Jordan approved adding one property on 2026-09-05; the saola-panel
session was told it is additive and that it must not read the property
until the release that ships it is announced. This is the first
sanctioned change to the frozen interface since Stage 9.

**Contract (binding — add it to the Frozen external contracts section
in this file, to README.md's frozen-contract section, and move it out of
the "Planned" note in `contrib/notifications/README.md`; ASD-STE100
prose, `ste100 check` on both docs):**

- `HasUrgent: b` on `io.saola.Notifications1`, emitting standard
  `PropertiesChanged` in `changed_properties` like the other four.
- `true` while history holds at least one notification with
  `Urgency::Critical` that has **not been seen**.
- A notification is *seen* when the centre opens after it arrived, or
  when it arrives while the centre is already open (it is on screen).
  Dismissal (`Dismiss`, `DismissAll`, the centre's own rows, history-cap
  eviction) removes the entry, so it cannot count.
- DND does **not** affect it — that is the point. Nothing else in the
  interface changes; names and semantics of the existing six methods and
  four properties stay exactly as served.

**Implementation (test-first, pure logic inline-tested, time injected —
AGENTS.md's resilience rules):**

1. `store.rs`: `Notification` gains `seen: bool` (false on `notify`).
   Add `Store::mark_history_seen(&mut self)` and a pure
   `Store::has_urgent(&self) -> bool`. Unit tests: critical unseen →
   true; critical then `mark_history_seen` → false; normal unseen →
   false; dismissed critical → false; critical evicted by the history
   cap → false; a second critical arriving after the first was seen →
   true again.
2. `main.rs`: every arm that *opens* the centre (`ToggleCentre` when
   closed, `OpenCentre`) calls `mark_history_seen` before
   `sync_control_state`. `on_notify` marks the new entry seen when
   `self.centre` is open. No new tick, no new subscription.
3. `dbus.rs`: `ControlState` gains `has_urgent: bool`;
   `ControlState::changed` diffs it (extend its unit test);
   `ControlService` gains `#[zbus(property)] fn has_urgent`;
   `sync_control_state` emits `has_urgent_changed` through the same
   `InterfaceRef` path the other four use. `Daemon::sync_control_state`
   computes it from `Store::has_urgent`.
4. Docs, as listed under Contract above, plus AGENTS.md's status block
   (one short paragraph: Stage 11 done, what changed, that the panel
   session still needs the release tag).

Manual evidence for the handoff: in the nested-niri recipe from
AGENTS.md, `notify-send -u critical` with DND on → `busctl --user
get-property … HasUrgent` is `true` and `busctl --user monitor` shows
`PropertiesChanged {HasUrgent: true}`; `ToggleCentre` → `false`. Do
**not** message saola-panel from this stage — the contract-change
message goes out with the release tag, which is Jordan's call. Put the
ready-to-send text (property name, semantics, the tag placeholder) in
the handoff so the release can paste it.
