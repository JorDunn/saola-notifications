//! `notifications.toml` — the user-facing daemon defaults, resolved once at
//! boot ([`NotificationsConfig::load`]) and again on every edit via
//! `config_watch`'s live reload.
//!
//! # Why `toml`, and why by hand (teaching note)
//!
//! [`toml::Table`] is walked explicitly here — `table.get("history-cap")`,
//! `.as_integer()`, … — instead of deriving `serde::Deserialize` on
//! [`NotificationsConfig`] itself. This is a project-wide rule (AGENTS.md's
//! Config bullet), for two reasons: a reader newer to Rust can trace an
//! explicit walk line by line instead of trusting a derive macro's hidden
//! codegen, and a hand-written extractor can name exactly *which* knob was
//! bad ("history-cap -3 is not a non-negative integer") in a way a one-shot
//! "deserialize failed" error cannot. `toml`'s own default features do pull
//! in `serde` — the crate uses it internally to give `Table`/`Value` their
//! own `Deserialize` impls — but that is `toml` deserializing into its own
//! generic value tree, not this module deriving anything on
//! `NotificationsConfig`.
//!
//! # No wrapper table
//!
//! `notifications.toml` is this app's own file (nothing else reads it), so
//! every knob is a bare top-level key (`history-cap = 100`, not
//! `[notifications]\nhistory-cap = 100`) — one less level of nesting to walk
//! and to hand-write, matching `saola-capture`'s `capture.toml`. The
//! reserved `[apps]` table (post-v0.1 per-app rules, PLAN.md Stage 2) is the
//! one exception: it is a real sub-table so a future stage can nest rules
//! under an app name, but this module parses nothing out of it yet — an
//! `[apps]` table present in the file today is silently ignored, not warned
//! about (it isn't a mistake, it's the file matching the sample schema
//! early).
//!
//! # Resilience rules (binding — AGENTS.md's Config bullet)
//!
//! - **No file at all** → [`NotificationsConfig::default`], silently. The
//!   expected case for anyone who hasn't written a `notifications.toml` yet.
//! - **File present but not valid TOML** ("garbage") → one `tracing::warn!`
//!   naming the file and the parse error, then the whole config falls back
//!   to [`NotificationsConfig::default`] — not a partial merge. A document
//!   that doesn't even parse gives this module nothing safe to partially
//!   trust.
//! - **File parses, but one knob's *value* is nonsense** (`history-cap =
//!   "lots"`, say) → warn on that one knob (via `tracing::warn!`, per
//!   AGENTS.md — unlike `saola-capture`/`saola-panel`'s `eprintln!`, this
//!   crate already owns a `tracing` subscriber from Stage 1's
//!   `init_tracing`), keep parsing the rest of the document, and default
//!   just that knob. A typo in `history-cap` must not blank out
//!   `dnd-default`.
//!
//! Every path is unit-tested below.

use std::fmt;
use std::path::{Path, PathBuf};

use toml::Table;

/// The default `history-cap` — the maximum number of notifications kept in
/// the in-memory history (README's Schema section, PLAN.md's v0.1 keys).
pub const DEFAULT_HISTORY_CAP: usize = 100;

/// The whole of `notifications.toml`, resolved to typed values. v0.1's three
/// keys only — the `[apps]` table is reserved but unparsed (see the module
/// doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationsConfig {
    /// `dnd-default = false` — whether manual do-not-disturb starts on at
    /// boot. Toggled at runtime from the notification centre or
    /// `io.saola.Notifications1`'s `SetDnd` — this is only the startup
    /// value.
    pub dnd_default: bool,
    /// `history-cap = 100` — the in-memory history's maximum entry count.
    /// Oldest entries drop first once the cap is reached (Stage 4's job).
    pub history_cap: usize,
    /// `critical-bypasses-dnd = true` — whether a critical-urgency
    /// notification shows as a toast even while manual DND is on. Never
    /// applies to auto-DND while `saola-capture` records (AGENTS.md's DND
    /// policy bullet — that bypass is not configurable).
    pub critical_bypasses_dnd: bool,
}

impl Default for NotificationsConfig {
    /// Manual DND off, a 100-entry history, critical urgency bypasses
    /// manual DND — the values PLAN.md's Stage 2 task and the README's
    /// Schema section both name. This is also what an absent
    /// `notifications.toml` produces, and what a garbage one falls back to
    /// in full.
    fn default() -> Self {
        NotificationsConfig {
            dnd_default: false,
            history_cap: DEFAULT_HISTORY_CAP,
            critical_bypasses_dnd: true,
        }
    }
}

/// A TOML document that failed to parse at all — the one `Err`
/// [`NotificationsConfig::parse`] produces. Every other problem (an absent
/// knob, a bad knob value) resolves to a default and is reported via
/// `tracing::warn!` instead — see the module doc comment's resilience
/// rules.
#[derive(Debug)]
pub struct ConfigError(toml::de::Error);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl NotificationsConfig {
    /// Where `notifications.toml` lives: the resolved config **directory**
    /// joined with the fixed file name. Resolution order, most-specific
    /// first (AGENTS.md's Config bullet — this daemon has no CLI flags, so
    /// unlike `saola-capture`'s `--config-dir`, there is no per-run
    /// override rung here):
    ///
    /// 1. **`$SAOLA_CONFIG_DIR`** — the Saola desktop's own env var.
    /// 2. **`$XDG_CONFIG_HOME/saola`** — the XDG base-directory spec.
    /// 3. **`~/.config/saola`** — the spec's own fallback for an unset
    ///    `$XDG_CONFIG_HOME`.
    ///
    /// `None` only when nothing in the chain resolves (no Saola or XDG var,
    /// and no `$HOME`) — treated the same as "no file": defaults.
    pub fn resolve_path() -> Option<PathBuf> {
        config_dir_from(
            std::env::var_os("SAOLA_CONFIG_DIR"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .map(|dir| dir.join("notifications.toml"))
    }

    /// Load the config at boot from the path [`Self::resolve_path`] gave the
    /// caller (`None` loads pure defaults — a container/CI environment with
    /// no `$HOME` is "no config is possible here", not "broken config").
    /// Never fails: every error path warns and returns a value, never
    /// propagating a `Result` up to `main`.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        Self::load_from(path)
    }

    fn load_from(path: &Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            // Covers both "the file doesn't exist" (the common case) and
            // any other I/O error (permissions, …) — both degrade to
            // defaults silently, same as the panel's and capture's loaders.
            Err(_) => return Self::default(),
        };
        match Self::parse(&contents) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    "{} is not valid TOML ({err}) — using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Re-read the config for a **live reload** (`config_watch` calls this
    /// after inotify reports the file changed). `None` means "keep whatever
    /// config the daemon is already running" — a malformed edit must not
    /// blank the running config back to defaults mid-save.
    pub fn reload_from(path: &Path) -> Option<Self> {
        let contents = match std::fs::read_to_string(path) {
            // A file that vanished between the inotify event and this read
            // (a delete, or a save that hasn't landed the new inode yet) is
            // "no config" — defaults, same as `load_from`'s missing-file
            // case, not "keep the stale config".
            Err(_) => return Some(Self::default()),
            Ok(contents) => contents,
        };
        match Self::parse(&contents) {
            Ok(config) => Some(config),
            Err(err) => {
                tracing::warn!(
                    "{} is not valid TOML ({err}) — keeping the current config",
                    path.display()
                );
                None
            }
        }
    }

    /// Parse a `notifications.toml` document's contents into a
    /// [`NotificationsConfig`].
    ///
    /// Returns `Err` **only** if `contents` isn't valid TOML at all — every
    /// other problem (an absent knob, a bad knob value) resolves to a
    /// default and is reported via `tracing::warn!`. This is the function
    /// the unit tests below exercise directly.
    pub fn parse(contents: &str) -> Result<Self, ConfigError> {
        let body: Table = contents.parse().map_err(ConfigError)?;

        let dnd_default = read_bool(&body, "dnd-default").unwrap_or(false);
        let history_cap = read_history_cap(&body).unwrap_or(DEFAULT_HISTORY_CAP);
        let critical_bypasses_dnd = read_bool(&body, "critical-bypasses-dnd").unwrap_or(true);

        Ok(NotificationsConfig {
            dnd_default,
            history_cap,
            critical_bypasses_dnd,
        })
    }
}

/// The testable core of [`NotificationsConfig::resolve_path`]'s directory
/// chain: takes every environment variable as a plain argument instead of
/// reading the environment itself, so precedence can be unit-tested without
/// mutating (and thereby racing every other test in this binary against)
/// the process's real environment — never `std::env::set_var` in a test,
/// per AGENTS.md's Testing bullet. Identical logic to `saola-capture`'s and
/// `saola-panel`'s own `config_dir_from`: an env var set to the **empty
/// string** is treated as unset and falls through to the next rung (the
/// XDG spec's own rule for `$XDG_CONFIG_HOME`, applied uniformly to
/// `$SAOLA_CONFIG_DIR` too).
fn config_dir_from(
    saola: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(saola) = saola
        && !saola.is_empty()
    {
        return Some(PathBuf::from(saola));
    }
    if let Some(xdg) = xdg
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("saola"));
    }
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/saola"))
}

/// `table.get(name)` as a `bool`. A present-but-non-boolean value
/// (`dnd-default = "yes"`, say) warns and falls back to that knob's
/// default, same per-knob rule every other bad value gets.
fn read_bool(table: &Table, name: &str) -> Option<bool> {
    let value = table.get(name)?;
    match value.as_bool() {
        Some(b) => Some(b),
        None => {
            tracing::warn!("notifications.toml: {name} {value} is not a boolean — using default");
            None
        }
    }
}

/// `history-cap = <count>` as a non-negative integer. Only TOML integers
/// qualify, and only non-negative ones — a negative history size means
/// nothing. Both warn and default, the same per-knob rule every other bad
/// value gets.
fn read_history_cap(table: &Table) -> Option<usize> {
    let value = table.get("history-cap")?;
    match value.as_integer() {
        Some(cap) if cap >= 0 => Some(cap as usize),
        _ => {
            tracing::warn!(
                "notifications.toml: history-cap {value} is not a non-negative integer — \
                 using default ({DEFAULT_HISTORY_CAP})"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse: defaults, full, partial ---------------------------------

    /// An empty document (also what `load_from` sees when the real file is
    /// missing and falls back before ever calling `parse`) yields exactly
    /// the hardcoded defaults.
    #[test]
    fn default_config_parses() {
        let config = NotificationsConfig::parse("").expect("an empty document is valid TOML");
        assert_eq!(config, NotificationsConfig::default());
    }

    /// Every v0.1 knob PLAN.md lists, set to non-default values, all land
    /// correctly. Bare top-level keys, no `[notifications]` wrapper table.
    #[test]
    fn full_config_parses() {
        let toml = r#"
            dnd-default = true
            history-cap = 250
            critical-bypasses-dnd = false
        "#;
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");

        assert!(config.dnd_default);
        assert_eq!(config.history_cap, 250);
        assert!(!config.critical_bypasses_dnd);
    }

    /// A config that only overrides one knob leaves the rest at their
    /// defaults — knob-by-knob fallback, not "any knob present disables all
    /// defaults".
    #[test]
    fn partial_config_parses() {
        let toml = "history-cap = 50";
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");

        assert_eq!(config.history_cap, 50);
        assert!(!config.dnd_default);
        assert!(config.critical_bypasses_dnd);
    }

    /// The reserved `[apps]` table parses (it's valid TOML) but contributes
    /// nothing yet — the post-v0.1 schema placeholder PLAN.md's Stage 2 task
    /// calls for.
    #[test]
    fn reserved_apps_table_is_ignored_not_rejected() {
        let toml = r#"
            dnd-default = true

            [apps]
            # future per-app rules live here
        "#;
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert!(config.dnd_default);
        assert_eq!(config.history_cap, DEFAULT_HISTORY_CAP);
    }

    // -- one-bad-knob degradation ----------------------------------------

    #[test]
    fn nonsense_dnd_default_falls_back_to_default_and_keeps_the_rest() {
        let toml = r#"
            dnd-default = "sure"
            history-cap = 42
        "#;
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert!(
            !config.dnd_default,
            "a non-boolean dnd-default keeps the default"
        );
        assert_eq!(
            config.history_cap, 42,
            "a bad dnd-default must not blank history-cap"
        );
    }

    #[test]
    fn nonsense_critical_bypasses_dnd_falls_back_to_default() {
        let toml = r#"critical-bypasses-dnd = "always""#;
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert!(config.critical_bypasses_dnd);
    }

    #[test]
    fn negative_history_cap_falls_back_to_default() {
        let toml = "history-cap = -1";
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.history_cap, DEFAULT_HISTORY_CAP);
    }

    #[test]
    fn fractional_history_cap_falls_back_to_default() {
        let toml = "history-cap = 12.5";
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.history_cap, DEFAULT_HISTORY_CAP);
    }

    #[test]
    fn non_integer_history_cap_falls_back_to_default() {
        let toml = r#"history-cap = "lots""#;
        let config = NotificationsConfig::parse(toml).expect("well-formed TOML");
        assert_eq!(config.history_cap, DEFAULT_HISTORY_CAP);
    }

    #[test]
    fn history_cap_accepts_zero() {
        let config = NotificationsConfig::parse("history-cap = 0").expect("well-formed TOML");
        assert_eq!(config.history_cap, 0, "a zero-entry history is valid");
    }

    /// Syntactically invalid TOML is the one case `parse` itself rejects.
    #[test]
    fn garbage_is_rejected_by_parse() {
        let result = NotificationsConfig::parse("this is not = valid [[[ toml");
        assert!(result.is_err());
    }

    // -- load_from / reload_from: filesystem round trips ------------------

    /// `load_from`'s fallback path, exercised end to end against a temp
    /// file: a malformed file degrades to full defaults.
    #[test]
    fn garbage_file_falls_back_to_defaults() {
        let path = std::env::temp_dir().join(format!(
            "saola-notifications-test-garbage-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "this is not = valid [[[ toml").unwrap();

        let config = NotificationsConfig::load_from(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(config, NotificationsConfig::default());
    }

    /// A missing file is not an error at all — same defaults, no crash.
    #[test]
    fn missing_file_falls_back_to_defaults() {
        let path = std::env::temp_dir().join(format!(
            "saola-notifications-test-definitely-missing-{}.toml",
            std::process::id()
        ));
        std::fs::remove_file(&path).ok();

        let config = NotificationsConfig::load_from(&path);

        assert_eq!(config, NotificationsConfig::default());
    }

    /// `reload_from` on a well-formed edit returns the new config.
    #[test]
    fn reload_from_a_well_formed_edit_returns_the_new_config() {
        let path = std::env::temp_dir().join(format!(
            "saola-notifications-test-reload-ok-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "history-cap = 5").unwrap();

        let reloaded = NotificationsConfig::reload_from(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(
            reloaded,
            Some(NotificationsConfig {
                history_cap: 5,
                ..NotificationsConfig::default()
            })
        );
    }

    /// `reload_from` on a mid-save garbage file keeps the running config —
    /// `None`, not defaults. This is the behavior that distinguishes it
    /// from `load_from`/boot: a bad edit must never flash a live daemon
    /// back to defaults.
    #[test]
    fn reload_from_a_malformed_edit_keeps_the_running_config() {
        let path = std::env::temp_dir().join(format!(
            "saola-notifications-test-reload-garbage-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "this is not = valid [[[ toml").unwrap();

        let reloaded = NotificationsConfig::reload_from(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(reloaded, None);
    }

    /// `reload_from` on a file that vanished between the inotify event and
    /// the read (a delete) resolves to defaults, not "keep the running
    /// config" — the config no longer exists to keep.
    #[test]
    fn reload_from_a_deleted_file_resolves_to_defaults() {
        let path = std::env::temp_dir().join(format!(
            "saola-notifications-test-reload-deleted-{}.toml",
            std::process::id()
        ));
        std::fs::remove_file(&path).ok();

        let reloaded = NotificationsConfig::reload_from(&path);

        assert_eq!(reloaded, Some(NotificationsConfig::default()));
    }

    // -- resolve_path precedence ------------------------------------------

    #[test]
    fn config_dir_precedence_saola_over_xdg_and_home() {
        let dir = config_dir_from(
            Some("/saola/dir".into()),
            Some("/xdg/dir".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/saola/dir")));
    }

    #[test]
    fn config_dir_precedence_xdg_over_home() {
        let dir = config_dir_from(None, Some("/xdg/dir".into()), Some("/home/jordan".into()));
        assert_eq!(dir, Some(PathBuf::from("/xdg/dir/saola")));
    }

    #[test]
    fn config_dir_falls_back_to_home() {
        let dir = config_dir_from(None, None, Some("/home/jordan".into()));
        assert_eq!(dir, Some(PathBuf::from("/home/jordan/.config/saola")));
    }

    #[test]
    fn config_dir_empty_env_vars_are_treated_as_unset() {
        // `VAR=` in a shell one-liner clears a variable, not names a
        // directory — the XDG spec's own rule, applied uniformly.
        let dir = config_dir_from(
            Some("".into()),
            Some("".into()),
            Some("/home/jordan".into()),
        );
        assert_eq!(dir, Some(PathBuf::from("/home/jordan/.config/saola")));
    }

    #[test]
    fn config_dir_none_when_nothing_resolves() {
        let dir = config_dir_from(None, None, None);
        assert_eq!(dir, None);
    }
}
