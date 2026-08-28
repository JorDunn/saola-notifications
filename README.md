# saola-notifications

The notification system for [Saola](https://github.com/JorDunn/saola-theme), a
Linux desktop environment built in Rust (iced 0.14 + zbus) that targets the
[niri](https://github.com/YaLTeR/niri) Wayland compositor. One binary serves
three roles: the freedesktop notification daemon
(`org.freedesktop.Notifications`), the toast popup stack, and the
notification centre.

## Status

Pre-v0.1, skeleton stage. The staged build plan is [PLAN.md](PLAN.md); agent
conventions are in [AGENTS.md](AGENTS.md). No daemon, D-Bus service, or UI
surface exists yet — `cargo run` resolves and loads `notifications.toml`,
logs the result, and exits. The UI follows the
[Saola style guide](docs/SAOLA-STYLE-GUIDE.md).

## Building

```bash
cargo build
cargo build --release
```

Standard `cargo build`/`cargo test`/`cargo clippy --all-targets -- -D
warnings`/`cargo fmt --check` — see [AGENTS.md](AGENTS.md)'s Commands
section for the full verify sequence CI runs.

## Running

```bash
cargo run
```

Right now `cargo run` needs no Wayland session at all — it only resolves and
loads `notifications.toml` and exits. Once the daemon lands (Stage 5) it
will need a real niri session, or `niri` started inside a window for
isolated testing (see AGENTS.md). There is no CLI surface yet; later
stages add D-Bus service startup, then the toast and notification centre
surfaces.

## Configuring

`notifications.toml`, hand-walked over `toml::Table` — never
`#[derive(Deserialize)]`, so one bad setting only warns and falls back to
its own default; the rest of the file still applies. No file present means
silent defaults; an unparseable file prints one warning and starts with
defaults anyway. The app never fails to start over a bad config.

**Resolution chain** (first match wins; an environment variable set to the
empty string counts as unset):

1. `$SAOLA_CONFIG_DIR`
2. `$XDG_CONFIG_HOME/saola`
3. `~/.config/saola`

The file itself is `<that directory>/notifications.toml`.

### Schema

Live-reload watches the resolved config directory: edit `notifications.toml`
while the daemon runs and the change applies without a restart (once the
daemon itself lands in Stage 5 — today `cargo run` only loads the file once,
at startup).

```toml
# Do not disturb by default at startup (toggle at runtime from the
# notification centre or the control D-Bus interface).
dnd-default = false

# Maximum number of notifications kept in the in-memory history. Oldest
# entries drop first once the cap is reached.
history-cap = 100

# Let a critical-urgency notification show as a toast even while manual
# do-not-disturb is on. Never applies to auto-DND while saola-capture is
# recording — a critical toast is never burned into a screencast.
critical-bypasses-dnd = true

# Reserved for a future release — per-app notification rules are not read
# yet.
# [apps]
```

## Architecture, for contributors

One process, `iced_layershell::build_pattern::daemon`, booting with zero
surfaces and spawning `Toasts` or `Centre` on demand. One tokio runtime
shared by iced and zbus. Full binding rules — surface geometry, the D-Bus
bridge shape, name-claim posture, the module pattern, resilience rules, and
the zero-hardcoded-style rule with its theme-gap protocol — live in
[AGENTS.md](AGENTS.md); that document, not this README, is the source of
truth for contributing code. The staged build plan and its frozen external
contracts (`org.freedesktop.Notifications`, `io.saola.Notifications1`, the
consumed `io.saola.Capture1` signals) are in [PLAN.md](PLAN.md).

### `io.saola.Notifications1` (frozen contract)

`io.saola.Notifications1` is the control interface for the saola-panel
indicator. This part of the document gives the method names, the property
names, and the correct output for each one. Get agreement from Jordan
before you make a name or an output here different. Other software uses
this interface at this time.

The daemon controls this interface at the object path
`/io/saola/Notifications1`.

**Methods**

- `ToggleCentre()` — Open the notification centre when it is closed. Close
  the notification centre when it is open.
- `OpenCentre()` — Open the notification centre. Do not act when the
  notification centre is open.
- `CloseCentre()` — Close the notification centre. Do not act when the
  notification centre is closed.
- `SetDnd(b)` — Set manual do-not-disturb to the value `b`. This command
  does not set auto-DND. Auto-DND is the do-not-disturb condition that
  starts with no command from an operator, while saola-capture records
  the screen.
- `Dismiss(u id)` — Remove one notification, named by `id`, from the toast
  stack and from history. If no notification has that `id`, this command
  does not act. This command gives no error.
- `DismissAll()` — Remove all notifications from the toast stack and from
  history.

Each dismissal through this interface sends a `NotificationClosed(id, 2)`
signal from `org.freedesktop.Notifications`. This interface sends one
signal for each notification it removes. The value `2` in this signal
shows that an operator asked to dismiss the notification.

**Properties**

Each property below sends the standard D-Bus `PropertiesChanged` signal
at each time its value changes.

- `NotificationCount: u` — The number of notifications in history at this
  time. This is the number the saola-panel indicator shows as a badge.
  History holds at most `history-cap` notifications (see Schema, above).
  When history holds `history-cap` notifications, one saved notification
  drops out each time a new notification comes in. Then this number stops
  rising.
- `DndActive: b` — This property is `true` when do-not-disturb applies at
  this time, from `DndManual` or from an active saola-capture recording.
- `DndManual: b` — This property is `true` when an operator sets
  do-not-disturb, through `SetDnd` or through the toggle in the
  notification centre.
- `CentreOpen: b` — This property is `true` when the notification centre
  is open at this time.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
