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

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
