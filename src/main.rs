//! `saola-notifications` entry point.
//!
//! Stage 1 built the repo skeleton and dependency survey; Stage 2 added
//! `notifications.toml` support. Stage 3 (this file) adds the D-Bus bridge
//! (`src/dbus.rs`) — this is still a **headless** stage: `main` is a plain
//! `#[tokio::main]` runner, not the `iced_layershell::build_pattern::
//! daemon` boot sequence AGENTS.md's Architecture section describes. That
//! sequence, with its zero-boot-surfaces daemon and `Subscription::run`
//! worker, arrives in Stage 5; until then this file's whole job is to keep
//! the D-Bus bridge alive and prove it works via `tracing` output and
//! manual `busctl`/`notify-send` evidence (see the Stage 3 handoff).
//!
//! # Two tasks, not one (teaching note)
//!
//! `dbus::run` is spawned as its own task rather than `.await`ed inline,
//! and a *separate* loop here drains the event channel — deliberately two
//! tasks, not one. `dbus::run` never returns on the happy path (see its
//! own doc comment: it awaits `std::future::pending()` to keep the D-Bus
//! connection alive), so if this file needs to react to `DaemonEvent`s
//! *and* keep the bridge running, spawning is not optional — `.await`ing
//! `dbus::run` directly would mean this task never reaches the drain loop
//! below it. Stage 5 replaces this whole shape with the real one:
//! `dbus::run`'s connect-and-serve logic becomes a `Subscription::run`
//! worker feeding `iced::Daemon::update` directly, and this manual
//! spawn/drain split goes away.
//!
//! Still nothing else is real: no notification store (Stage 4), no
//! layershell surfaces (Stage 5).

mod config;
mod config_watch;
mod dbus;

use futures::StreamExt;
use iced::futures::channel::mpsc;

/// Sets up `tracing-subscriber`'s `fmt` layer on stderr (which systemd
/// already journals per-unit for the packaged
/// `contrib/systemd/saola-notifications.service`) with `RUST_LOG`-driven
/// verbosity via `env-filter`, defaulting to `"info"` when `RUST_LOG` is
/// unset or invalid. Lifted from `saola-session::main::init_tracing` — same
/// shape, same defaulting rule.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() {
    init_tracing();
    tracing::info!("saola-notifications: starting (Stage 3 skeleton — headless D-Bus bridge)");

    let config_path = config::NotificationsConfig::resolve_path();
    let config = config::NotificationsConfig::load(config_path.as_deref());
    tracing::info!(
        ?config_path,
        dnd_default = config.dnd_default,
        history_cap = config.history_cap,
        critical_bypasses_dnd = config.critical_bypasses_dnd,
        "resolved notifications.toml"
    );

    // A small bounded channel: every served D-Bus method only ever offers
    // to it via `try_send` (never blocks a bus reply on this loop keeping
    // up — see `dbus.rs`'s own doc comment), so a full channel degrades to
    // a logged, dropped event rather than backing up method calls. `8`
    // matches `saola-capture::dbus_worker_stream`'s own bound for the same
    // shape of channel.
    let (events_tx, mut events_rx) = mpsc::channel::<dbus::DaemonEvent>(8);

    tokio::spawn(dbus::run(events_tx));

    // Drains every `DaemonEvent` for the rest of the process's life. This
    // loop ends only when every `Sender` clone is dropped — in practice,
    // only if `dbus::run` hit an unrecoverable setup error and returned
    // (see its doc comment); on the happy path it never returns, so
    // neither does this.
    while let Some(event) = events_rx.next().await {
        tracing::info!(?event, "saola-notifications: daemon event");
    }

    tracing::warn!(
        "saola-notifications: the D-Bus event channel closed — the bridge is no longer running; \
         exiting"
    );
}
