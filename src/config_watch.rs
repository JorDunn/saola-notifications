//! Live-reload for `notifications.toml`: watch the file on disk, and hand
//! the daemon a freshly parsed [`NotificationsConfig`] whenever it changes
//! — so a config edit reaches the running daemon without a restart.
//!
//! Copied from `saola-panel`'s `src/config_watch.rs` (PLAN.md Stage 2 task:
//! "copy … the inotify with the rename/inode caveat it documents, adapt
//! the filename") — the mechanics below are unchanged from that module;
//! only the config type and file name differ.
//!
//! # The signal (and why this isn't the poll AGENTS.md forbids)
//!
//! The kernel's inotify(7) interface *pushes* file-change events: the
//! worker below is asleep in `stream.next().await` until the kernel has
//! something to say, exactly like the D-Bus modules (Stage 3+) will be
//! asleep in their signal streams. Nothing here ticks, and an untouched
//! config file costs the daemon nothing for the whole life of the process.
//! (The one `sleep` below is a debounce that only ever runs *after* an
//! event has already arrived — gated, not standing.)
//!
//! # Watch the directory, not the file (teaching note)
//!
//! An inotify watch follows an **inode**, not a path. Most editors save
//! "atomically": write the new content to a temp file, then `rename(2)` it
//! over `notifications.toml` — which replaces the inode, so a watch on the
//! file itself goes quiet after the very first save. Watching the parent
//! directory (`~/.config/saola/`) instead means every way the file can
//! change arrives as a directory event carrying the file's *name* —
//! `CLOSE_WRITE` for an in-place save, `MOVED_TO` for the atomic rename,
//! `CREATE`/`DELETE` for the file appearing or going away — and the name
//! filter below picks out the ones about `notifications.toml`.
//!
//! # Debounce
//!
//! One human "save" is several kernel events (vim's atomic save is a temp
//! file plus a rename; some editors truncate, write, and close in separate
//! syscalls). Reloading on each would apply a half-written file. So the
//! first relevant event starts a short grace period, everything that
//! arrives during it is drained and discarded, and the file is read once
//! at the end — by which point the save has finished.
//!
//! # Resilience (the absent-service rule, applied to a directory)
//!
//! *Which* file to watch is decided by [`NotificationsConfig::resolve_path`]
//! and handed in through [`subscription`]; an environment where nothing in
//! that chain resolves gets no subscription at all. No config *directory*
//! at boot → the watch can't be established, a single `tracing::warn!` says
//! live-reload is off, and the worker parks forever — the daemon runs
//! exactly as before this module existed. What the reload does with a
//! malformed file is [`NotificationsConfig::reload_from`]'s contract: keep
//! the running config, never flash to defaults mid-edit.
//!
//! # Why this module is inert in Stage 2 (teaching note)
//!
//! [`subscription`] returns an `iced::Subscription` — a *description* of
//! work, not work itself. It only starts running once handed to a live
//! `iced_layershell::build_pattern::daemon`'s own `subscription()` method
//! and driven by iced's runtime (see `saola-panel::main`'s wiring for the
//! shape). This crate's `main.rs` doesn't boot that daemon until Stage 5,
//! so nothing in this module runs yet — clippy's `dead_code` lint would
//! otherwise fire on every item here, hence the module-level `allow`
//! below. Stage 5 removes it once `main.rs` actually calls
//! [`subscription`] from the app's own `subscription()` method.

#![allow(
    dead_code,
    reason = "wired by Stage 5's iced_layershell daemon subscription() — see module doc comment"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::{FutureExt, SinkExt, Stream, StreamExt};
use inotify::{EventMask, Inotify, WatchMask};

use crate::config::NotificationsConfig;

/// What the watcher produces — nested into the daemon's outer `Message` enum
/// as `Message::ConfigReloaded(config_watch::Message)` once Stage 5 wires it
/// up, the same pattern AGENTS.md's Module pattern section describes for
/// every module.
#[derive(Debug, Clone)]
pub enum Message {
    /// The config file changed and parsed: here is the whole new
    /// [`NotificationsConfig`], resolved. Carrying the finished value
    /// (rather than a bare "something changed" ping) keeps the file I/O and
    /// parsing on the worker, off the UI thread.
    Reloaded(NotificationsConfig),
}

/// The watcher as an iced subscription, for the `notifications.toml` path
/// the caller resolved (`NotificationsConfig::resolve_path` — the
/// `$SAOLA_CONFIG_DIR` / XDG chain). Taking the *resolved* path rather than
/// re-deriving it here is what guarantees the watcher and the boot loader
/// can never disagree about which file is the config.
///
/// Identity mechanics: `Subscription::run_with` keys on the fn pointer
/// *plus* the `data` value, so iced would tear the worker down and spin up
/// a fresh one if the path ever changed between `subscription`
/// recomputations. Here it never does — the environment is fixed at exec
/// time — so in practice the key buys the same one-worker-forever guarantee
/// `Subscription::run` gives a bare subscription, while letting the worker
/// receive an argument at all (a bare `run` fn cannot).
pub fn subscription(path: &Path) -> Subscription<Message> {
    Subscription::run_with(path.to_path_buf(), watch_stream)
}

/// How long after the first change event the reload waits for the save to
/// finish (see the module doc comment's debounce section). Long enough to
/// cover any editor's multi-syscall save; far too short to feel like lag on
/// a human timescale.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// The worker: establish the directory watch, then loop forever turning
/// bursts of change events into at most one reload each.
///
/// The `&PathBuf` parameter (clippy would rather see `&Path`) is not a
/// stylistic slip: [`Subscription::run_with`]'s `builder` parameter is
/// `fn(&D) -> S`, and `D` here is `PathBuf` (the type [`subscription`]
/// hands `run_with` as `data`) — a `fn(&Path) -> S` is a different,
/// non-matching type, so `&Path` would not compile. (Same note, same
/// reason, as `saola-panel`'s own `watch_stream`.)
#[allow(
    clippy::ptr_arg,
    reason = "must match Subscription::run_with's fn(&D) -> S exactly, where D = PathBuf"
)]
// `+ use<>` is Rust 2024 edition's opt-out of that edition's new default:
// return-position `impl Trait` now auto-captures every in-scope lifetime
// (here, `path`'s `'_`) unless told not to, which would otherwise tie the
// returned stream's type to the caller's borrow and break the higher-ranked
// `fn(&D) -> S` coercion `Subscription::run_with` needs (`&PathBuf` gets
// cloned to an owned `PathBuf` on the very first line of the body below —
// the stream never actually borrows from the input, so opting out is
// correct, not just a workaround). `saola-panel` doesn't need this because
// it's still on edition 2021, where RPIT only captured a lifetime that
// appeared explicitly in the trait bounds.
fn watch_stream(path: &PathBuf) -> impl Stream<Item = Message> + use<> {
    let path = path.clone();
    iced::stream::channel(4, async move |mut sender: mpsc::Sender<Message>| {
        // The path always has a parent (`…/notifications.toml` under some
        // resolved directory, by `NotificationsConfig::resolve_path`'s
        // construction), but destructure rather than unwrap — a defensive
        // posture this worker can afford, since "no watch" is a legal
        // outcome. Park (rather than return) so the subscription stays
        // formally alive without iced re-running it.
        let Some(dir) = path.parent() else {
            iced::futures::future::pending::<()>().await;
            return;
        };
        let Some(file_name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
            iced::futures::future::pending::<()>().await;
            return;
        };

        // The four ways the file's content can change under its name, per
        // the module doc comment: in-place save, atomic-rename save,
        // created fresh, deleted. `MOVED_FROM` covers `mv notifications.
        // toml elsewhere`, which is a deletion from this directory's point
        // of view. The two `_SELF` marks are about the watched *directory*
        // itself going away — without them the kernel would still drop the
        // watch (delivering only an unnamed `IGNORED` this loop's name
        // filter would swallow), and live-reload would die with no trace;
        // catching them explicitly is what turns that into a `tracing::
        // warn!` line (see the loop below).
        let mask = WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_TO
            | WatchMask::MOVED_FROM
            | WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF;

        let inotify = match Inotify::init() {
            Ok(inotify) => inotify,
            Err(err) => {
                tracing::warn!("inotify unavailable ({err}) — live-reload disabled");
                iced::futures::future::pending::<()>().await;
                return;
            }
        };
        if let Err(err) = inotify.watches().add(dir, mask) {
            // The common cause: ~/.config/saola doesn't exist yet. One
            // line, then quiet — the absent-service contract.
            tracing::warn!(
                "cannot watch {} ({err}) — live-reload disabled",
                dir.display()
            );
            iced::futures::future::pending::<()>().await;
            return;
        }

        // The buffer inotify parses events out of. 4 KiB fits dozens of
        // directory events per read; a config directory sees a handful per
        // save.
        let mut stream = match inotify.into_event_stream([0u8; 4096]) {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!("inotify stream failed ({err}) — live-reload disabled");
                iced::futures::future::pending::<()>().await;
                return;
            }
        };

        while let Some(event) = stream.next().await {
            let Ok(event) = event else { continue };
            // The watched directory itself was deleted or renamed out from
            // under us. The kernel has already dropped the watch (an
            // `IGNORED` follows), so no future edit can ever wake this
            // worker again — say so once, then park, the same posture as a
            // directory that was absent at boot. (Re-establishing the
            // watch on a recreated directory would mean polling for it to
            // reappear, which is exactly what this module must not do.)
            if event
                .mask
                .intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF)
            {
                tracing::warn!(
                    "{} is gone — live-reload disabled until restart",
                    dir.display()
                );
                iced::futures::future::pending::<()>().await;
                return;
            }
            // Directory events name the child they concern; skip everything
            // that isn't about notifications.toml (the temp files of an
            // atomic save, sibling configs, …).
            if event.name.as_deref() != Some(file_name.as_os_str()) {
                continue;
            }

            // Debounce: let the save finish, then drain whatever else it
            // queued so a three-event save is one reload, not three.
            // `now_or_never` polls the next-event future exactly once —
            // `Some` means an event was already waiting (discard it and ask
            // again), `None` means the queue is empty and we can read the
            // settled file.
            tokio::time::sleep(DEBOUNCE).await;
            while let Some(Some(_)) = stream.next().now_or_never() {}

            if let Some(config) = NotificationsConfig::reload_from(&path) {
                // An `Err` here means the receiving side is gone — the app
                // is shutting down — so the worker's job is over either
                // way.
                if sender.send(Message::Reloaded(config)).await.is_err() {
                    return;
                }
            }
        }
    })
}
