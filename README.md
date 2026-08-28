# saola-notifications

The notification system for [Saola](https://github.com/JorDunn/saola-theme), a
Linux desktop environment built in Rust (iced 0.14 + zbus) that targets the
[niri](https://github.com/YaLTeR/niri) Wayland compositor. One binary serves
three roles: the freedesktop notification daemon
(`org.freedesktop.Notifications`), the toast popup stack, and the
notification centre.

## Status

Version 0.1. All planned features work: the freedesktop notification
daemon, the toast stack, the notification centre, and a bridge that
turns saola-capture signals into native toasts and turns on
do-not-disturb during a recording. History exists in memory only in
version 0.1. A restart clears it. The user interface follows the
[Saola style guide](docs/SAOLA-STYLE-GUIDE.md). [PLAN.md](PLAN.md) has
the staged build plan. [AGENTS.md](AGENTS.md) has agent conventions.
[docs/REVIEW-v0.1.md](docs/REVIEW-v0.1.md) has the version 0.1 review.
One open point from that review: **no test has confirmed that the toast
or the notification centre draw the way the style guide describes.**
See "A known limitation," below.

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
cargo run --release
```

This command needs a niri compositor session and a D-Bus session bus.
The daemon asks to own the name `org.freedesktop.Notifications`, the
standard name every notifying app calls. When another notification
daemon — mako or dunst — owns that name first, this daemon writes a log
line and keeps running under its own name only. The control interface,
the capture bridge, and the toast and centre surfaces work either way. A
second copy of this daemon exits at once with a normal exit code,
instead of fighting the first copy for its own bus name,
`io.saola.Notifications1`.

For a packaged install, `contrib/systemd/saola-notifications.service`
is a `systemd --user` unit. The unit starts the daemon under a niri
session only (see the unit file for the exact rule), and restarts the
daemon after a failure. `contrib/aur/PKGBUILD` builds and installs the
program, the unit, and a link that turns the unit on with no extra
step.

### Isolated tests (nested niri)

To try the daemon without a real session, run niri inside a window.
Point the daemon and a test tool such as `notify-send` or `busctl` at
one shared D-Bus session bus:

```bash
niri &                                  # a niri window opens; its own log names its Wayland socket
export WAYLAND_DISPLAY=wayland-2        # whatever that log printed
dbus-run-session -- bash -c '
  ./target/release/saola-notifications &
  notify-send "Hello" "A test notification"
  niri msg layers                       # lists layer-shell surfaces this daemon created
'
```

`niri msg layers` shows only that a surface exists, and where. It does
not show what, if anything, the surface draws. See "A known
limitation," below.

## Configuring

`notifications.toml`. The daemon reads this file by hand, key by key —
never through Rust's `#[derive(Deserialize)]`. One bad setting drops to
its own default and prints one warning; every other setting in the
file still applies. A missing file gives every default with no warning.
A file the parser cannot read at all gives every default with one
warning. A bad config never stops the daemon from starting.

**Resolution chain** (first match wins; an environment variable set to the
empty string counts as unset):

1. `$SAOLA_CONFIG_DIR`
2. `$XDG_CONFIG_HOME/saola`
3. `~/.config/saola`

The path resolved this way, plus `notifications.toml`, names the file.

### Schema

The daemon watches the config directory while it runs. Edit
`notifications.toml`, and the change takes effect at once, with no
restart. An edit the parser cannot read gives one warning and changes
nothing — the daemon keeps the config it had before the edit.

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

### `org.freedesktop.Notifications` (frozen contract)

This is the standard freedesktop notification-daemon interface. Every
notifying app speaks this interface now — `notify-send`, web browsers,
chat clients — no matter which desktop it runs on. This daemon serves
the interface as the freedesktop specification defines it. This
section names only the exact values this daemon returns, not the full
specification.

The daemon serves this interface at the object path
`/org/freedesktop/Notifications`. It serves this interface only while
it owns the matching bus name (see "Running," above, for the other
case).

- `GetCapabilities() -> as` — Returns the list `["body", "actions",
  "icon-static", "persistence"]`. This daemon does not support
  `body-markup`. An app may send Pango or HTML markup in a
  notification's body text. This daemon removes that markup before
  it shows the text.
- `GetServerInformation() -> (ssss)` — Returns `("saola-notifications",
  "Saola", <this daemon's version number>, "1.2")`. The value `"1.2"`
  names the version of the freedesktop specification this daemon
  implements. This value does not change with the daemon's version
  number.
- `Notify(...) -> u` — Accepts a new notification and returns its `id`.
  When a caller sets `replaces_id` to an `id` in use, this daemon
  returns that same `id`.
- `CloseNotification(u id)` — Removes one notification, named by `id`,
  from the toast stack, and sends `NotificationClosed(id, 3)`.
- `NotificationClosed(u id, u reason)` — A signal this daemon sends
  each time a notification leaves the toast stack. `reason` is `1`
  when the notification's timeout ran out, `2` when an operator
  dismissed it by hand (a card click, an action pill, or a dismissal
  through `io.saola.Notifications1`), or `3` after a
  `CloseNotification` call.
- `ActionInvoked(u id, s action_key)` — A signal this daemon sends
  when an operator picks one of a notification's action pills, or
  clicks a card that carries a `"default"` action.

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

## A known limitation

No test has confirmed, on a real screen, that a toast card or the
notification centre draw the way the [style
guide](docs/SAOLA-STYLE-GUIDE.md) describes them. Every development
stage tested this daemon inside a nested niri window, for safety (see
AGENTS.md). In that test, a surface the daemon creates on demand shows
up in `niri msg layers` with the right name, place, size, and keyboard
behavior. The surface draws no visible frame. [docs/REVIEW-v0.1.md](docs/
REVIEW-v0.1.md) has more detail: an earlier build setting left this
daemon with no working graphics driver in a debug build. Stage 10 fixed
that setting (see that document's finding C-1). The fix explains a
blank screen for every debug build up to now. The fix does not explain
why a release build of a sibling program, `saola-capture`, drew a
blank screen too, in the same test.

**Run this daemon on a real niri session. Confirm a notification shows
up before you trust any part of its visual design.** If a notification
does not show up, a fault remains in how the daemon creates a surface
after startup, not at startup the way `saola-panel` creates its own
surfaces. That fault needs its own study.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
