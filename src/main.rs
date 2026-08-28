//! `saola-notifications` entry point.
//!
//! Stage 1 built the repo skeleton and dependency survey; Stage 2 (this
//! stage) adds `notifications.toml` support (see `PLAN.md`) — the stub
//! below now resolves and loads the config at boot and logs the result, so
//! the verify command (`cargo fmt --check && cargo clippy ... && cargo
//! test`) is green and the two new modules are reachable from `main`
//! (nothing else compiles them into the binary yet). Still nothing else is
//! real: no D-Bus (Stage 3), no notification store (Stage 4), no
//! layershell surfaces (Stage 5). Later stages replace `main`'s body with
//! the `iced_layershell::build_pattern::daemon` boot sequence AGENTS.md's
//! Architecture section describes — a daemon that starts with zero
//! surfaces and spawns `Toasts`/`Centre` on demand; that daemon's own
//! `subscription()` method is what will actually drive `config_watch`'s
//! live reload (see that module's doc comment for why it is inert until
//! then).

mod config;
mod config_watch;

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

fn main() {
    init_tracing();
    tracing::info!("saola-notifications: starting (Stage 2 skeleton — config only, no daemon yet)");

    let config_path = config::NotificationsConfig::resolve_path();
    let config = config::NotificationsConfig::load(config_path.as_deref());
    tracing::info!(
        ?config_path,
        dnd_default = config.dnd_default,
        history_cap = config.history_cap,
        critical_bypasses_dnd = config.critical_bypasses_dnd,
        "resolved notifications.toml"
    );
}
