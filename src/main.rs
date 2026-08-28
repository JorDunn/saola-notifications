//! `saola-notifications`: the Saola desktop's freedesktop notification
//! daemon, toast stack and (Stage 7) notification centre — one binary, one
//! process, one runtime.
//!
//! # The process shape (teaching note)
//!
//! This file boots an `iced_layershell::build_pattern::daemon`: a winit-style
//! event loop that owns the process for its whole life and drives every async
//! thing this daemon does through its own `tokio` executor. That is AGENTS.md's
//! "one runtime" rule in practice — nothing here ever constructs a
//! `tokio::Runtime`, and there is no `#[tokio::main]` any more (Stages 2–4 had
//! one; the D-Bus bridge that lived under it is now [`dbus_worker_stream`], an
//! `iced::Subscription`).
//!
//! # Zero surfaces at boot (teaching note)
//!
//! [`Daemon::boot`] maps nothing. A notification daemon spends almost all of
//! its life with nothing on screen, and a layer-shell surface that exists but
//! shows nothing still swallows pointer events across its whole declared area
//! (see [`Daemon::sync_toast_surface`]). The event loop is kept alive with no
//! surfaces by `StartMode::Background` — `layershellev`'s run loop only stops
//! itself when `units.is_empty() && !is_allscreens() && !is_background()`, so
//! the background mode is what makes a surfaceless daemon legal at all. The
//! toast surface is then mapped on the first card and unmapped after the last
//! one leaves.
//!
//! # Where the clock is read
//!
//! Twice, both in this file: [`Daemon::update`] stamps `Instant::now()` on the
//! event it is handling, and [`Daemon::view`] reads it to place each card in
//! its animation. Nothing in `store.rs` or `modules/toast.rs` ever reads the
//! clock — that is what makes the expiry and animation math unit-testable
//! (AGENTS.md's Resilience rules).

mod config;
mod config_watch;
mod dbus;
mod modules;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::Space;
use iced::{Element, Subscription, Task, window};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use saola_theme::Theme;

fn main() -> ExitCode {
    init_tracing();
    run_daemon()
}

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

fn run_daemon() -> ExitCode {
    // The one `Theme` built before the daemon exists, purely so `default_font`
    // can be computed from it. `Daemon::boot` builds its own
    // (`Theme::saola()`); this crate has no config knob that overrides the
    // palette, so there is nothing to thread between the two beyond staying in
    // sync by construction — the same note `saola-capture::run_daemon` carries.
    let theme = Theme::saola();
    let default_font = saola_theme::convert::ui_font(&theme);

    let result = daemon(
        Daemon::boot,
        "saola-notifications",
        Daemon::update,
        Daemon::view,
    )
    .subscription(Daemon::subscription)
    .theme(Daemon::theme)
    // Transparent app-wide background. Without it iced clears every surface
    // to `to_iced_theme`'s own `background` (`palette.ink`) before drawing,
    // so a rounded, fading card would be composited over an opaque ink
    // rectangle the size of the whole surface instead of over the wallpaper.
    // `saola-capture` found this the hard way with `grim`; copied rather than
    // rediscovered.
    .style(Daemon::style)
    .settings(Settings {
        // `default_font` must live *inside* this literal, never behind a
        // separate `.default_font(..)` builder call before `.settings(..)`:
        // that call's effect is clobbered by the `..Default::default()` in
        // whichever literal lands last, and this one lands last.
        // (`saola-panel::main`'s comment on the same field ordering.)
        default_font,
        layer_settings: LayerShellSettings {
            // None of these matter for a `Background`-mode surface — it is a
            // bare `wl_surface` with no shell role at all, so there is no
            // anchor to stick to and nothing to grab focus. They are spelled
            // out rather than left to `..Default::default()` so a future
            // change here cannot quietly inherit a value nobody chose.
            anchor: Anchor::empty(),
            layer: Layer::Background,
            exclusive_zone: 0,
            size: None,
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::None,
            events_transparent: true,
            // The one field that matters: see this module's doc comment on
            // why zero-surface boot needs `Background`.
            start_mode: StartMode::Background,
        },
        ..Default::default()
    })
    .run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "saola-notifications: the daemon event loop failed");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------

/// Registry of live layer-shell surfaces, keyed by iced's `window::Id`, and
/// what each one is for — the shape `saola-panel::main::SurfaceRole`
/// established and AGENTS.md's Architecture section fixes at two roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRole {
    /// The toast stack (`modules::toast`). Mapped by
    /// [`Daemon::sync_toast_surface`] on the first card, resized by
    /// unmap-then-respawn whenever the stack's declared height changes, and
    /// unmapped once the last card leaves.
    Toasts,
    /// The notification centre (Stage 7). The registry arm exists now so
    /// that [`Daemon::view`] has a total match over the roles this daemon
    /// will have, rather than growing a new one later; nothing spawns a
    /// centre surface yet.
    #[allow(dead_code, reason = "Stage 7 spawns the first centre surface")]
    Centre,
}

/// The toast surface's layer-shell settings, sized for the stack's current
/// declared height (`modules::toast::stack_height`).
///
/// Geometry is §6's, taken from tokens: `sizes.notification_card_width`
/// wide, `sizes.popover_top` below the screen top (the same offset
/// saola-panel's popovers use, so a toast never collides with the panel) and
/// `sizes.panel_margin_islands` in from the right edge — 26 px, §6's "26px
/// from the relevant edge", via the one token that carries that number as a
/// screen-edge inset (there is no dedicated `popover_right`; recorded in
/// `docs/UPSTREAM-THEME-DEBT.md`).
///
/// `exclusive_zone: 0` reserves nothing but still lets the compositor keep
/// the surface clear of anyone else's reserved strip.
/// `KeyboardInteractivity::None` is binding (AGENTS.md): a toast must never
/// take the keyboard. `events_transparent: false` is the opposite — the card
/// has to receive hover and clicks — which is precisely why the surface is
/// resized to fit its content; see [`Daemon::sync_toast_surface`].
fn toast_surface_settings(theme: &Theme, height: u32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        anchor: Anchor::Top | Anchor::Right,
        layer: Layer::Overlay,
        size: Some((theme.sizes.notification_card_width.round() as u32, height)),
        margin: Some((
            theme.sizes.popover_top.round() as i32,
            theme.sizes.panel_margin_islands.round() as i32,
            0,
            0,
        )),
        exclusive_zone: Some(0),
        keyboard_interactivity: KeyboardInteractivity::None,
        events_transparent: false,
        // Named so `niri msg layers` can tell this surface apart during a
        // live check — the only introspection command that lists layer-shell
        // surfaces at all.
        namespace: Some("saola-notifications-toasts".to_string()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------

/// The daemon's whole state.
#[derive(Debug)]
struct Daemon {
    theme: Theme,
    /// `notifications.toml` as last loaded or reloaded.
    config: config::NotificationsConfig,
    /// Where that file is, if it resolved — held so `subscription` can watch
    /// it. `None` means no config directory resolved at all, in which case
    /// there is nothing to watch and defaults stand for the whole run.
    config_path: Option<PathBuf>,
    /// The theme-and-config values `store.rs` needs, resolved once at boot.
    /// `history_cap` is refreshed on a config reload; everything else is
    /// theme-derived and fixed for the process's life.
    limits: store::Limits,
    /// The notification model: toast stack, capped history, collapsed groups.
    store: store::Store,
    /// The toast surface's own view state (which card the pointer is in).
    toasts: modules::toast::Toasts,
    windows: HashMap<window::Id, SurfaceRole>,
    /// The toast surface's Id, while one is mapped.
    toast_surface: Option<window::Id>,
    /// The declared height the *currently mapped* toast surface was spawned
    /// for. Compared against the stack's current height on every sync, so a
    /// card arriving or leaving is noticed even though the surface's own Id
    /// does not change on its own. See [`Daemon::sync_toast_surface`].
    toast_surface_height: u32,
    /// The session-bus connection, once [`dbus_worker_stream`] has one. Held
    /// so `update` can emit signals through it (see [`Daemon::emit_closed`]);
    /// `None` until `BusReady` arrives, and every emitter degrades to a
    /// logged warning rather than a panic in that window.
    connection: Option<zbus::Connection>,
    /// Manual do-not-disturb (`io.saola.Notifications1.SetDnd`), seeded from
    /// `notifications.toml`'s `dnd-default`.
    dnd_manual: bool,
    /// Auto-DND while saola-capture is recording — Stage 8 sets it; nothing
    /// does yet. `effective_dnd = manual || recording` (AGENTS.md), and
    /// critical urgency bypasses the manual half only, never this one.
    recording_dnd: bool,
}

impl Daemon {
    /// The boot closure: load config, resolve limits, map **nothing**.
    fn boot() -> (Self, Task<Message>) {
        let theme = Theme::saola();
        let config_path = config::NotificationsConfig::resolve_path();
        let config = config::NotificationsConfig::load(config_path.as_deref());
        tracing::info!(
            ?config_path,
            dnd_default = config.dnd_default,
            history_cap = config.history_cap,
            critical_bypasses_dnd = config.critical_bypasses_dnd,
            "saola-notifications: resolved notifications.toml"
        );

        let limits = limits_from(&theme, &config);
        let daemon = Self {
            dnd_manual: config.dnd_default,
            theme,
            config,
            config_path,
            limits,
            store: store::Store::new(),
            toasts: modules::toast::Toasts::default(),
            windows: HashMap::new(),
            toast_surface: None,
            toast_surface_height: 0,
            connection: None,
            recording_dnd: false,
        };

        (daemon, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shutdown(reason) => {
                reason.log();
                // `iced::exit()` produces `Action::Exit`, which
                // `iced_layershell`'s event loop turns into
                // `ReturnData::RequestExit`; `run_daemon`'s `.run()` then
                // returns `Ok(())` and the process exits 0. That is the
                // "second instance exits cleanly" posture AGENTS.md fixes,
                // and it is why nothing in this crate calls
                // `std::process::exit` from inside a stream any more (Stage
                // 3's `dbus::run` did; it is gone).
                iced::exit()
            }

            Message::BusReady(connection) => {
                self.connection = Some(connection);
                Task::none()
            }

            Message::Notify(request) => self.on_notify(request),

            // `dbus.rs`'s `close_notification` has already emitted
            // `NotificationClosed(id, 3)` itself — the one reason a method
            // call is always entitled to claim. All that is left here is to
            // take the card off the screen; `store.rs` never touches D-Bus,
            // so nothing does this automatically.
            Message::CloseNotification(id) => {
                self.store.dismiss_toast(id);
                self.sync_toast_surface()
            }

            Message::Dismiss(id) => {
                if self.store.dismiss_toast(id) {
                    let emit = self.emit_closed(&[id], store::CloseReason::UserDismissed);
                    Task::batch([emit, self.sync_toast_surface()])
                } else {
                    Task::none()
                }
            }

            Message::DismissAll => {
                let ids = self.store.dismiss_all_toasts();
                if ids.is_empty() {
                    return Task::none();
                }
                let emit = self.emit_closed(&ids, store::CloseReason::UserDismissed);
                Task::batch([emit, self.sync_toast_surface()])
            }

            Message::SetDnd(manual) => {
                self.dnd_manual = manual;
                tracing::info!(
                    manual,
                    effective = store::effective_dnd(manual, self.recording_dnd),
                    "saola-notifications: do-not-disturb changed"
                );
                Task::none()
            }

            // Stage 7 owns the centre surface. Logged rather than silently
            // dropped so a `busctl` call against the frozen control interface
            // still shows evidence that it landed.
            Message::ToggleCentre | Message::OpenCentre | Message::CloseCentre => {
                tracing::info!(
                    "saola-notifications: the notification centre arrives in Stage 7 — the \
                     control method was received and had no surface to act on"
                );
                Task::none()
            }

            Message::Toast(inner) => {
                let now = Instant::now();
                let action = self
                    .toasts
                    .update(inner, &mut self.store, &self.limits, now);
                let emit = match action {
                    modules::toast::Action::None => Task::none(),
                    modules::toast::Action::Closed { ids, reason } => {
                        self.emit_closed(&ids, reason)
                    }
                    // Stage 6: `ActionInvoked` is unconditional; the store
                    // has already decided (and, if `closed`, already
                    // applied) whether the toast also comes off screen —
                    // `Toasts::update`'s own `invoke` helper called
                    // `store::invoke_action_policy` before returning this.
                    modules::toast::Action::Invoked { id, key, closed } => {
                        let invoked = self.emit_action_invoked(id, key);
                        if closed {
                            let dismissed =
                                self.emit_closed(&[id], store::CloseReason::UserDismissed);
                            Task::batch([invoked, dismissed])
                        } else {
                            invoked
                        }
                    }
                };
                Task::batch([emit, self.sync_toast_surface()])
            }

            Message::Config(config_watch::Message::Reloaded(config)) => {
                tracing::info!(
                    history_cap = config.history_cap,
                    critical_bypasses_dnd = config.critical_bypasses_dnd,
                    "saola-notifications: notifications.toml reloaded"
                );
                // `dnd-default` is deliberately *not* re-applied: it seeds
                // manual DND at boot, and silently flipping the user's live
                // DND state because they edited an unrelated key would be a
                // surprise. `history-cap` takes effect on the next push
                // rather than retroactively trimming what is already held.
                self.limits.history_cap = config.history_cap;
                self.config = config;
                Task::none()
            }

            // The variants `#[to_layer_message(multi)]` injects
            // (`NewLayerShell`, `RemoveWindow`, `SizeChange`, …) are handled
            // by the runtime itself and never reach here — the same catch-all
            // `saola-panel::main::Panel::update` ends with, for the same
            // reason.
            _ => Task::none(),
        }
    }

    /// One `Notify` call, all the way from parsed request to a card on
    /// screen. The exact call sequence Stage 4's handoff specifies.
    fn on_notify(&mut self, request: NotifyRequest) -> Task<Message> {
        let now = Instant::now();
        let suppress = store::should_suppress_toast(
            request.urgency,
            self.dnd_manual,
            self.recording_dnd,
            self.config.critical_bypasses_dnd,
        );
        let replaces_id = request.replaces_id;
        let notification = request.into_notification(now);
        // Captured before the move below — Stage 6 evidence (`busctl … Notify`
        // with an `actions` array) reads this count off the log rather than
        // off a pixel, since a pill can't be aimed at in the nested-niri
        // environment this daemon's manual testing runs in (see the Stage 5
        // handoff's "pixels" section).
        let action_count = notification.actions.len();
        let effect = self
            .store
            .notify(notification, replaces_id, suppress, now, &self.limits);

        tracing::debug!(
            ?effect,
            suppress,
            action_count,
            "saola-notifications: notification stored"
        );

        // Style guide §6: a replaced card "resets the clock" — and a reset
        // stopwatch is a *running* one. If the pointer is sitting still on
        // that card, no further `Hovered` will ever arrive (the pointer did
        // not move, so it never re-entered anything), so the card would
        // silently resume counting down under a hovering pointer. Re-pausing
        // whatever the pointer is on closes that hole; `pause_toast` is a
        // no-op for an id that is not on screen or is already paused.
        if let Some(hovered) = self.toasts.hovered() {
            self.store.pause_toast(hovered, now);
        }

        self.sync_toast_surface()
    }

    /// Every `id` here is one this daemon spawned itself (registered
    /// synchronously in [`Self::spawn_surface`]) or the surfaceless
    /// `Background` boot surface from `run_daemon`'s `Settings`, which is
    /// never registered and falls through to the empty arm.
    fn view(&self, id: window::Id) -> Element<'_, Message> {
        match self.windows.get(&id) {
            Some(SurfaceRole::Toasts) => self
                .toasts
                .view(&self.theme, &self.store, Instant::now())
                .map(Message::Toast),
            // Stage 7.
            Some(SurfaceRole::Centre) | None => Space::new().into(),
        }
    }

    /// Independent workers, batched: the D-Bus bridge, the toast stack's
    /// gated animation tick, and the `notifications.toml` watcher.
    fn subscription(&self) -> Subscription<Message> {
        let config = match &self.config_path {
            Some(path) => config_watch::subscription(path).map(Message::Config),
            // No config directory resolved at all — there is no file to
            // watch, and defaults stand for the whole run.
            None => Subscription::none(),
        };

        Subscription::batch([
            Subscription::run(dbus_worker_stream),
            self.toasts.subscription(&self.store).map(Message::Toast),
            config,
        ])
    }

    /// One `Theme` for every surface — no per-surface palette.
    fn theme(&self, _id: window::Id) -> iced::Theme {
        saola_theme::to_iced_theme(&self.theme)
    }

    /// See `run_daemon`'s `.style(..)` comment: this must be transparent.
    fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: iced::Color::TRANSPARENT,
            ..iced::theme::default(theme)
        }
    }

    /// Ask the compositor for a new layer-shell surface in `role`, and record
    /// the role against the Id that surface will have.
    ///
    /// # How the daemon learns a spawned surface's Id (teaching note)
    ///
    /// It does not learn it — it chooses it. `Message::layershell_open` is
    /// one of the constructors `#[to_layer_message(multi)]` generates; it
    /// calls `window::Id::unique()` itself and hands back that Id alongside a
    /// `Task` that asks the runtime for the surface. Because the Id exists
    /// before the surface does, the role is recorded in the very same
    /// `update` call that requests it — there is no window in which `view`
    /// could be called with an Id this registry cannot classify.
    fn spawn_surface(
        &mut self,
        role: SurfaceRole,
        settings: NewLayerShellSettings,
    ) -> (window::Id, Task<Message>) {
        let (id, task) = Message::layershell_open(settings);
        self.windows.insert(id, role);
        (id, task)
    }

    /// Ask the compositor to destroy `id`, and forget its role.
    fn remove_surface(&mut self, id: window::Id) -> Task<Message> {
        self.windows.remove(&id);
        Task::done(Message::RemoveWindow(id))
    }

    /// Map, resize, or unmap the toast surface so its declared size always
    /// matches what the stack actually draws.
    ///
    /// # Resizing is unmap-then-respawn, not a live `SizeChange` (teaching note)
    ///
    /// A layer-shell surface with `events_transparent: false` — which the
    /// toast surface must be, since a card is clickable — takes pointer input
    /// across its **entire** declared area, drawn or not. A surface left
    /// sized for three cards while one is showing would silently swallow
    /// every click in the empty space below that card, which is most of the
    /// time. Respawning at exactly `modules::toast::stack_height`'s answer
    /// for the current stack keeps the clickable footprint equal to the
    /// painted one.
    ///
    /// The respawn is invisible: the surface holds no keyboard focus, and no
    /// animation state lives on it (every stopwatch is in [`store::Store`],
    /// which a new surface reads exactly as the old one did).
    ///
    /// Keyed on *height* rather than card count because Stage 6's action
    /// pills make two stacks of the same length different heights.
    fn sync_toast_surface(&mut self) -> Task<Message> {
        let needed = modules::toast::stack_height(&self.theme, self.store.toasts());
        tracing::debug!(
            cards = self.store.toasts().len(),
            needed,
            mapped = ?self.toast_surface,
            mapped_height = self.toast_surface_height,
            "saola-notifications: toast surface sync"
        );

        match (self.toast_surface, needed) {
            (None, 0) => Task::none(),
            (None, height) => {
                let settings = toast_surface_settings(&self.theme, height);
                let (id, task) = self.spawn_surface(SurfaceRole::Toasts, settings);
                self.toast_surface = Some(id);
                self.toast_surface_height = height;
                task
            }
            (Some(id), 0) => {
                self.toast_surface = None;
                self.toast_surface_height = 0;
                self.remove_surface(id)
            }
            (Some(id), height) if height != self.toast_surface_height => {
                self.toast_surface_height = height;
                let remove = self.remove_surface(id);
                let settings = toast_surface_settings(&self.theme, height);
                let (new_id, spawn) = self.spawn_surface(SurfaceRole::Toasts, settings);
                self.toast_surface = Some(new_id);
                Task::batch([remove, spawn])
            }
            // Already mapped at the right size.
            _ => Task::none(),
        }
    }

    /// Emit `NotificationClosed(id, reason)` for each id, off the update
    /// thread.
    ///
    /// # Why `Task::future` and not an `.await` here (teaching note)
    ///
    /// `update` is synchronous — it cannot await anything. `Task::future` is
    /// how an iced app says "run this to completion on the runtime, and tell
    /// me nothing back": the future is moved onto iced's own executor (the
    /// single shared `tokio` runtime), and `.discard()` throws away its
    /// output so the task can join a `Task<Message>` batch without needing a
    /// no-op message variant to produce.
    ///
    /// The connection is cloned into the future rather than borrowed —
    /// `zbus::Connection` is an `Arc` inside, so the clone is a refcount
    /// bump, and a `'static` future cannot borrow `self`.
    fn emit_closed(&self, ids: &[u32], reason: store::CloseReason) -> Task<Message> {
        let Some(connection) = self.connection.clone() else {
            tracing::warn!(
                ?ids,
                "saola-notifications: no session-bus connection yet — NotificationClosed was not \
                 emitted (the card still left the screen)"
            );
            return Task::none();
        };

        let ids = ids.to_vec();
        let reason = reason.as_u32();
        Task::future(async move {
            for id in ids {
                if let Err(err) = dbus::emit_notification_closed(&connection, id, reason).await {
                    tracing::warn!(
                        id,
                        reason,
                        error = %err,
                        "saola-notifications: could not emit NotificationClosed"
                    );
                }
            }
        })
        .discard()
    }

    /// Emit `ActionInvoked(id, key)`, off the update thread — Stage 6's
    /// sibling of [`Self::emit_closed`]; see that method's doc comment for
    /// why `Task::future(..).discard()` rather than an `.await`.
    fn emit_action_invoked(&self, id: u32, key: String) -> Task<Message> {
        let Some(connection) = self.connection.clone() else {
            tracing::warn!(
                id,
                key,
                "saola-notifications: no session-bus connection yet — ActionInvoked was not \
                 emitted"
            );
            return Task::none();
        };

        Task::future(async move {
            if let Err(err) = dbus::emit_action_invoked(&connection, id, &key).await {
                tracing::warn!(
                    id,
                    key,
                    error = %err,
                    "saola-notifications: could not emit ActionInvoked"
                );
            }
        })
        .discard()
    }
}

/// The theme-and-config values `store.rs` works from. Built at boot, and its
/// `history_cap` refreshed on every config reload; see [`store::Limits`] for
/// what each one bounds.
fn limits_from(theme: &Theme, config: &config::NotificationsConfig) -> store::Limits {
    store::Limits {
        icon_tile: theme.sizes.icon_tile,
        toast_idle_ms: theme.motion.toast_idle,
        toast_envelope_ms: modules::toast::envelope(theme).as_millis() as u32,
        toast_max_stack: usize::from(theme.motion.toast_max_stack),
        history_cap: config.history_cap,
    }
}

// ---------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------

/// The daemon's message type.
///
/// `#[to_layer_message(multi)]` appends `iced_layershell`'s own layer-shell
/// control variants (`NewLayerShell`, `RemoveWindow`, `SizeChange`, …) and
/// implements the `TryInto<LayerShellCustomActionWithId>` conversion the
/// runtime requires. `multi` rather than the single-surface form because this
/// daemon spawns surfaces on demand and will run two roles at once (a toast
/// stack over an open centre).
#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    /// The daemon should stop, and why.
    Shutdown(ShutdownReason),
    /// [`dbus_worker_stream`] is serving, and here is the connection it
    /// serves on — stored so `update` can emit signals through it.
    BusReady(zbus::Connection),
    /// A `Notify` call, already parsed (hints resolved, markup stripped,
    /// image decoded) by the worker. See [`NotifyRequest`].
    Notify(NotifyRequest),
    /// `org.freedesktop.Notifications.CloseNotification(id)`.
    CloseNotification(u32),
    /// `io.saola.Notifications1.ToggleCentre` — Stage 7.
    ToggleCentre,
    /// `io.saola.Notifications1.OpenCentre` — Stage 7.
    OpenCentre,
    /// `io.saola.Notifications1.CloseCentre` — Stage 7.
    CloseCentre,
    /// `io.saola.Notifications1.SetDnd(b)` — manual do-not-disturb only.
    SetDnd(bool),
    /// `io.saola.Notifications1.DismissAll()`.
    DismissAll,
    /// `io.saola.Notifications1.Dismiss(id)`.
    Dismiss(u32),
    /// Wraps [`modules::toast::Message`] — the stack's tick, hover and click.
    Toast(modules::toast::Message),
    /// Wraps [`config_watch::Message`] — `notifications.toml` changed on disk
    /// and reparsed.
    Config(config_watch::Message),
}

/// One `Notify` call with everything already resolved except the moment it
/// arrived.
///
/// **Why the parsing happens in the worker, not in `update`.**
/// `store::parse_hints` decodes an `image-path` hint by reading a PNG off
/// disk, synchronously. Doing that in `update` would block the UI thread on
/// file I/O for every notification that carries an icon path (Stage 4's
/// handoff flagged exactly this). Doing it in [`dbus_worker_stream`] instead
/// puts it on a `tokio` worker thread, where a slow filesystem costs a late
/// toast rather than a stalled frame.
///
/// `posted_at` is the one field left unset, because `Instant::now()` is read
/// in `update` and nowhere else — see [`NotifyRequest::into_notification`].
#[derive(Debug, Clone)]
struct NotifyRequest {
    id: u32,
    replaces_id: u32,
    app_name: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<store::Action>,
    urgency: store::Urgency,
    image: Option<iced::widget::image::Handle>,
    expire_timeout: i32,
    transient: bool,
    resident: bool,
}

impl NotifyRequest {
    fn into_notification(self, now: Instant) -> store::Notification {
        store::Notification {
            id: self.id,
            app_name: self.app_name,
            app_icon: self.app_icon,
            summary: self.summary,
            body: self.body,
            actions: self.actions,
            urgency: self.urgency,
            image: self.image,
            expire_timeout: self.expire_timeout,
            transient: self.transient,
            resident: self.resident,
            posted_at: now,
        }
    }
}

/// Unpacks `Notify`'s `actions: as` argument.
///
/// The freedesktop spec packs actions as a **flat, alternating** array —
/// `["default", "Open", "cancel", "Cancel"]` — not a list of pairs, so a
/// well-formed array always has an even length. A trailing unpaired key (a
/// malformed client) is dropped rather than paired with an empty label: a
/// pill with no text on it is worse than a missing pill, and inventing a
/// label would put words in the sending application's mouth.
fn unpack_actions(flat: &[String]) -> Vec<store::Action> {
    flat.chunks_exact(2)
        .map(|pair| store::Action {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect()
}

#[derive(Debug, Clone)]
enum ShutdownReason {
    /// `io.saola.Notifications1` was already owned — AGENTS.md's name-claim
    /// posture: a second instance exits 0 rather than running two daemons.
    AlreadySecondInstance,
    /// The session bus itself was unreachable, or serving failed for a reason
    /// other than "already taken". Carries the error's own message; there is
    /// no more specific variant worth adding for what is an
    /// environment-level failure.
    BusUnavailable(String),
}

impl ShutdownReason {
    fn log(&self) {
        match self {
            ShutdownReason::AlreadySecondInstance => tracing::info!(
                "saola-notifications: another instance already owns {} — exiting cleanly rather \
                 than running two daemons",
                dbus::CONTROL_SERVICE_NAME
            ),
            ShutdownReason::BusUnavailable(err) => tracing::error!(
                error = %err,
                "saola-notifications: could not serve the D-Bus interfaces — exiting"
            ),
        }
    }
}

// ---------------------------------------------------------------------
// The D-Bus worker
// ---------------------------------------------------------------------

/// Connect to the session bus, serve both frozen interfaces, then hold the
/// connection open for the daemon's life while relaying every
/// [`dbus::DaemonEvent`] into a [`Message`].
///
/// # Why the loop *is* the lifetime (teaching note)
///
/// `zbus::Connection` is `#[must_use]` for a real reason: dropping the last
/// handle closes the socket and tears down every object the `ObjectServer`
/// holds. zbus dispatches each inbound method call on its own task, so
/// nothing here has to *poll* the connection — but something has to keep it
/// in scope. Looping on `events_rx.next()` does exactly that, and does useful
/// work at the same time. `events_tx` is never dropped (a clone lives inside
/// each served interface for as long as the object server does), so on the
/// happy path this loop never ends.
///
/// Stage 3's `dbus::run` parked on `std::future::pending()` here and called
/// `std::process::exit(0)` for the second-instance case. Both are gone: a
/// `process::exit` inside a subscription tears the process down mid-stream
/// instead of letting iced shut the event loop down cleanly, so the outcome
/// is reported as a `Message::Shutdown` and `update` calls `iced::exit()`.
fn dbus_worker_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(err) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::BusUnavailable(
                        err.to_string(),
                    )))
                    .await;
                return;
            }
        };

        // A small bounded channel: every served method only ever offers to it
        // with `try_send` (a bus reply is never blocked on this loop keeping
        // up — see `dbus.rs`), so a full channel degrades to a logged,
        // dropped event rather than backing method calls up.
        let (events_tx, mut events_rx) = mpsc::channel::<dbus::DaemonEvent>(8);

        match dbus::serve(&connection, events_tx).await {
            Ok(dbus::ServeOutcome::Serving {
                notifications_owned,
            }) => {
                if notifications_owned {
                    tracing::info!(
                        "saola-notifications: serving {} at {} and {} at {}",
                        dbus::NOTIFICATIONS_SERVICE_NAME,
                        dbus::NOTIFICATIONS_OBJECT_PATH,
                        dbus::CONTROL_SERVICE_NAME,
                        dbus::CONTROL_OBJECT_PATH
                    );
                } else {
                    tracing::info!(
                        "saola-notifications: {} is already owned by another notification daemon \
                         — no toasts will be raised from it; serving only {} at {}",
                        dbus::NOTIFICATIONS_SERVICE_NAME,
                        dbus::CONTROL_SERVICE_NAME,
                        dbus::CONTROL_OBJECT_PATH
                    );
                }

                if sender
                    .send(Message::BusReady(connection.clone()))
                    .await
                    .is_err()
                {
                    return;
                }

                // Read once, here, rather than per event: `parse_hints` needs
                // the icon-tile size to downsample a decoded image to, and
                // this worker has no access to `Daemon`'s own `Theme`.
                // `Theme::saola()` is a constant of the design system, so the
                // two can never disagree.
                let icon_tile = Theme::saola().sizes.icon_tile;

                while let Some(event) = events_rx.next().await {
                    let message = match event {
                        dbus::DaemonEvent::Notify {
                            id,
                            replaces_id,
                            app_name,
                            app_icon,
                            summary,
                            body,
                            actions,
                            hints,
                            expire_timeout,
                        } => {
                            let plain = dbus::hints_to_plain(&hints);
                            let parsed = store::parse_hints(&plain, &app_icon, icon_tile);
                            Message::Notify(NotifyRequest {
                                id,
                                replaces_id,
                                app_name,
                                app_icon,
                                // Clients send Pango markup regardless of what
                                // `GetCapabilities` advertises, so it is
                                // stripped on the way in rather than rendered
                                // as literal angle brackets.
                                summary: store::strip_markup(&summary),
                                body: store::strip_markup(&body),
                                actions: unpack_actions(&actions),
                                urgency: parsed.urgency,
                                image: parsed.image,
                                expire_timeout,
                                transient: parsed.transient,
                                resident: parsed.resident,
                            })
                        }
                        dbus::DaemonEvent::CloseNotification { id } => {
                            Message::CloseNotification(id)
                        }
                        dbus::DaemonEvent::ToggleCentre => Message::ToggleCentre,
                        dbus::DaemonEvent::OpenCentre => Message::OpenCentre,
                        dbus::DaemonEvent::CloseCentre => Message::CloseCentre,
                        dbus::DaemonEvent::SetDnd { manual } => Message::SetDnd(manual),
                        dbus::DaemonEvent::DismissAll => Message::DismissAll,
                        dbus::DaemonEvent::Dismiss { id } => Message::Dismiss(id),
                    };
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
            }
            Ok(dbus::ServeOutcome::AlreadySecondInstance) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::AlreadySecondInstance))
                    .await;
            }
            Err(err) => {
                let _ = sender
                    .send(Message::Shutdown(ShutdownReason::BusUnavailable(
                        err.to_string(),
                    )))
                    .await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_actions_unpacks_to_no_pills() {
        assert!(unpack_actions(&[]).is_empty());
    }

    #[test]
    fn the_flat_array_unpacks_into_key_label_pairs() {
        let actions = unpack_actions(&strings(&["default", "Open", "cancel", "Cancel"]));
        assert_eq!(
            actions,
            vec![
                store::Action {
                    key: "default".to_string(),
                    label: "Open".to_string()
                },
                store::Action {
                    key: "cancel".to_string(),
                    label: "Cancel".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_trailing_unpaired_key_is_dropped() {
        let actions = unpack_actions(&strings(&["yes", "Yes", "orphan"]));
        assert_eq!(
            actions,
            vec![store::Action {
                key: "yes".to_string(),
                label: "Yes".to_string()
            }],
            "an action key with no label is dropped, never given an invented one"
        );
    }

    #[test]
    fn limits_carry_the_style_guides_own_numbers() {
        let theme = Theme::saola();
        let config = config::NotificationsConfig::default();
        let limits = limits_from(&theme, &config);

        assert_eq!(limits.icon_tile, theme.sizes.icon_tile);
        assert_eq!(limits.toast_max_stack, 3, "§6: popups stack at most three");
        assert_eq!(
            limits.toast_idle_ms + limits.toast_envelope_ms,
            theme.motion.toast_total,
            "§5: a default toast's whole life is the theme's toast_total"
        );
        assert_eq!(limits.history_cap, config.history_cap);
    }
}
