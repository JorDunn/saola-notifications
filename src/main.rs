//! `saola-notifications` entry point.
//!
//! Stage 1 only builds the repo skeleton and dependency survey (see
//! `PLAN.md`) — this stub initializes logging and exits cleanly so the
//! verify command (`cargo fmt --check && cargo clippy ... && cargo test &&
//! cargo build`) is green. Nothing here is real yet: no D-Bus (Stage 3), no
//! notification store (Stage 4), no layershell surfaces (Stage 5). Later
//! stages replace `main`'s body with the `iced_layershell::build_pattern::
//! daemon` boot sequence AGENTS.md's Architecture section describes — a
//! daemon that starts with zero surfaces and spawns `Toasts`/`Centre` on
//! demand.

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
    tracing::info!("saola-notifications: starting (Stage 1 skeleton — no daemon yet)");
}
