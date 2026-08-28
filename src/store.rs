//! The pure notification model — hint parsing, markup stripping, expiry and
//! DND policy, and the in-memory toast/history store.
//!
//! # Hard constraints (PLAN.md Stage 4 — read before touching this file)
//!
//! - **No `zbus` imports.** The raw `Notify` hints arrive from `dbus.rs` as
//!   `HashMap<String, zbus::zvariant::OwnedValue>` — this file never sees
//!   that type. `dbus.rs` converts each `OwnedValue` into this module's own
//!   plain [`HintValue`] first (its `hint_value_from_owned`/`hints_to_plain`
//!   functions); [`parse_hints`] here only ever sees [`HintValue`]. This
//!   keeps the module dependency graph one-directional (`dbus.rs` depends on
//!   `store.rs`, never the reverse) and means every test in this file can
//!   build a `HintValue` by hand — no live bus, no zbus test harness.
//! - **No clock reads.** Every `Instant` this file touches is a parameter,
//!   never `Instant::now()` called internally — that is what keeps expiry
//!   and DND logic unit-testable without real time passing. `grep -n
//!   "Instant::now" src/store.rs` should always come back empty.
//! - **No iced widget code**, with one deliberate exception:
//!   `iced::widget::image::Handle` is the type [`Notification::image`] holds
//!   (decoded icon pixels), and it is also the type this file's own image
//!   decoders (`resolve_image` and friends, below) build directly via
//!   `Handle::from_rgba` — **never** `Handle::from_path`/`from_bytes`. Those
//!   two variants defer decoding to iced's renderer on a later frame
//!   (`saola-lockscreen::wallpaper`'s doc comment has the full,
//!   live-verified account, echoed in `saola-capture::modules::toast`'s own
//!   `thumbnail_handle`), which this crate's "decode is synchronous, decode
//!   failure is `None`, never an error" hint-parsing rule cannot tolerate —
//!   by the time hint parsing returns, the pixels must already be resolved
//!   one way or the other.
//!
//! # Layering (teaching note)
//!
//! Five roughly independent pieces live in this one file, in the order
//! below: the plain data model ([`Notification`], [`Action`], [`Urgency`],
//! [`CloseReason`]); hint parsing ([`HintValue`], [`parse_hints`], the image
//! decoders); the body markup stripper ([`strip_markup`]); DND policy
//! ([`effective_dnd`], [`should_suppress_toast`]); expiry policy
//! ([`Stopwatch`], [`ExpiryPolicy`], [`expiry_policy`], [`has_expired`]);
//! and finally the [`Store`] itself, which is the only piece that actually
//! holds state (the toast stack, the capped history `Vec`, the collapsed-
//! group set) and is where the replace-vs-same-app policy PLAN.md Stage 4
//! calls out lives. Everything above `Store` is a pure function or a pure
//! value type with no shared mutable state — `Store` is deliberately the
//! last thing in the file because it is built entirely out of the pieces
//! above it.
//!
//! # What Stage 5 does with this file
//!
//! See `.claude/handoffs/handoff_stage_4.md` for the exact call sites
//! (`DaemonEvent::Notify` → `Store::notify`, a tick subscription →
//! `Store::expire_toasts`, hover → `pause_toast`/`resume_toast`, and so on).
//! Nothing in this file constructs a `Subscription` or reads a clock itself
//! — that wiring is entirely Stage 5's job.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::widget::image;

// ============================================================================
// Limits — theme + config values injected at boot, never hardcoded here.
// ============================================================================

/// Every "how big" / "how long" / "how many" knob this file's policy
/// functions need, gathered in one place so nothing below ever hardcodes a
/// `3`, a `5000`, or a `100`.
///
/// Deliberately **not** `saola_theme::Theme` itself: this struct is plain
/// data (four fields, all `Copy`), so every test in this file builds one
/// inline (`Limits { icon_tile: 36.0, .. }`) without depending on the
/// `saola-theme` crate at all — Stage 5 is the one place that actually
/// reads `saola_theme::Theme::saola()` (`theme.sizes.icon_tile`,
/// `theme.motion.toast_idle`, `theme.motion.toast_max_stack`) and
/// `NotificationsConfig::history_cap` to build the real one at boot. See
/// the handoff for the exact token names/types this was checked against
/// (`saola-theme` tag `saola-theme-v0.13.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// `saola_theme::tokens::Sizes::icon_tile` — the notification card's
    /// icon tile, in logical pixels (`36.0` in the shipped theme). Bounds
    /// the *longer* side of every decoded image; see [`resolve_image`].
    pub icon_tile: f32,
    /// `saola_theme::tokens::Motion::toast_idle`, in milliseconds — the
    /// theme-default **rest** span an `expire_timeout` of `-1` resolves to.
    /// This is the phase where the card sits still and fully opaque; it is
    /// not the card's whole life (see [`Limits::toast_envelope_ms`]).
    pub toast_idle_ms: u32,
    /// `saola_theme::tokens::Motion::{toast_in + toast_out}`, in
    /// milliseconds — the entrance and exit animations that *bracket* the
    /// rest span.
    ///
    /// **Added in Stage 5**, and the reason is a correctness bug Stage 4
    /// could not see from where it stood: [`expiry_policy`] resolves the
    /// *rest* span (style guide §5's "5 s at rest"; PLAN.md's own words for
    /// a positive `expire_timeout` are "replaces the idle span"), but a card
    /// removed the instant its rest span ends never gets to play §5's
    /// one-second fade-out — it would vanish rather than leave. So the card
    /// is on screen for `toast_in + rest + toast_out`, and this field is the
    /// `toast_in + toast_out` half of that sum. Zero means "no animation
    /// bracket", which is what every expiry test written before Stage 5
    /// assumes.
    pub toast_envelope_ms: u32,
    /// `saola_theme::tokens::Motion::toast_max_stack` — at most this many
    /// toasts on screen at once; the next push evicts the oldest.
    pub toast_max_stack: usize,
    /// `NotificationsConfig::history_cap` — oldest history entries are
    /// dropped once this many are held.
    pub history_cap: usize,
}

// ============================================================================
// The data model
// ============================================================================

/// The three levels `NOTIFY_URGENCY` (byte `0`/`1`/`2`) can carry.
///
/// `Normal` is also the fallback for anything that isn't a recognized
/// `0`/`1`/`2` byte (missing hint, or a hint present with the wrong wire
/// type) — see [`parse_hints`]'s own doc comment for why "assume the least
/// special-cased behavior" is the right default here rather than treating a
/// malformed urgency hint as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

/// One `(action_key, localized_label)` pair from `Notify`'s `actions: as`
/// argument, which the freedesktop spec packs as a flat alternating array
/// (`["default", "Open", "cancel", "Cancel"]`) rather than a list of pairs.
/// Unpacking that flat array into this struct is Stage 5's job (it owns the
/// D-Bus-shaped `Vec<String>` in `DaemonEvent::Notify`) — this file only
/// defines the shape actions are stored in once unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// One notification, fully parsed: hints resolved, markup stripped, image
/// decoded (or not). Everything a toast card or a history row needs to
/// render is already sitting in a field here — nothing downstream re-parses
/// raw `Notify` arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub urgency: Urgency,
    pub image: Option<image::Handle>,
    pub expire_timeout: i32,
    pub transient: bool,
    pub resident: bool,
    pub posted_at: Instant,
}

/// The three reasons `NotificationClosed(id, reason)` can carry (freedesktop
/// spec). `dbus.rs`'s `close_notification` method already hardcodes `3`
/// for itself; [`Store::expire_toasts`] and a click-dismiss call site
/// (Stage 5) are what produce `1` and `2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    UserDismissed = 2,
    CloseNotification = 3,
}

impl CloseReason {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

// ============================================================================
// Actions (Stage 6 — "Actions")
// ============================================================================

/// The notification's `"default"` action, if it declared one.
///
/// Style guide §6 / PLAN.md Stage 6: `"default"` is the one action key that
/// never renders as a pill — it is what a card click fires instead of the
/// plain dismiss every other card gets. `main.rs`/`modules/toast.rs` call
/// this on every card click to decide which of the two a given card gets;
/// [`action_pills`] is the complementary "everything that *does* render" cut.
pub fn default_action(notification: &Notification) -> Option<&Action> {
    notification
        .actions
        .iter()
        .find(|action| action.key == "default")
}

/// The notification's action pills: every action except `"default"` (see
/// [`default_action`]). Order matches `Notify`'s own `actions` array, which
/// `main.rs`'s `unpack_actions` already preserves.
pub fn action_pills(notification: &Notification) -> impl Iterator<Item = &Action> {
    notification
        .actions
        .iter()
        .filter(|action| action.key != "default")
}

/// What invoking one action — a pill, or the card's own `"default"` action
/// firing on click — does to the toast that carried it.
///
/// PLAN.md Stage 6: "Invoking an action emits `ActionInvoked(id, key)` then
/// closes the toast (reason 2) unless the notification is `resident`."
/// `ActionInvoked` itself is unconditional (the caller always emits it;
/// this file never touches D-Bus) — this function decides only the "then
/// closes" half, so the toast module and `main.rs` share one answer instead
/// of each re-deriving it from `resident`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionInvocation {
    /// Whether the caller should also dismiss the toast
    /// (`store::dismiss_toast` + `NotificationClosed(id, 2)`) after
    /// `ActionInvoked` fires.
    pub close_after: bool,
}

pub fn invoke_action_policy(resident: bool) -> ActionInvocation {
    ActionInvocation {
        close_after: !resident,
    }
}

// ============================================================================
// Hint parsing
// ============================================================================

/// A `Notify` hint value, stripped down to exactly the wire shapes this
/// crate's hints actually use — the plain, `zbus`-free stand-in for
/// `zbus::zvariant::OwnedValue` this module's own doc comment promises.
/// `dbus.rs::hint_value_from_owned` is the only place that builds one from
/// a real bus value; every test in this file builds one directly.
///
/// Hint wire types this crate doesn't care about (`sender-pid`'s `i64`,
/// `category`'s `s` when unused, …) simply have no variant here —
/// `dbus.rs`'s conversion returns `None` for those, so they never make it
/// into the `HashMap<String, HintValue>` this file sees at all. That is a
/// deliberate, silent drop, not a bug: [`parse_hints`] only ever looks up a
/// small fixed set of keys by name, so an unconvertible or simply-unused
/// hint is inert either way.
#[derive(Debug, Clone, PartialEq)]
pub enum HintValue {
    /// `urgency`'s wire type (`y`, a single byte).
    Byte(u8),
    /// `transient`/`resident`'s wire type (`b`).
    Bool(bool),
    /// `image-path`/`image_path`'s wire type (`s`) — an absolute path or a
    /// `file://` URI (bare icon names are a known v0.1 limitation; see
    /// [`decode_path_str`]).
    Str(String),
    /// `image-data`/`image_data`/`icon_data`'s wire type, the freedesktop
    /// `iiibiiay` struct: `(width, height, rowstride, has_alpha,
    /// bits_per_sample, channels, data)`. Field names and order match the
    /// spec exactly — see [`decode_image_data`] for what happens to them.
    ImageData {
        width: i32,
        height: i32,
        rowstride: i32,
        has_alpha: bool,
        bits_per_sample: i32,
        channels: i32,
        data: Vec<u8>,
    },
}

/// Everything [`parse_hints`] resolves out of a `Notify` call's hints (plus
/// its `app_icon` argument, which is not a hint but participates in the
/// same image-lookup chain — see [`resolve_image`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedHints {
    pub urgency: Urgency,
    pub transient: bool,
    pub resident: bool,
    pub image: Option<image::Handle>,
}

/// Parses the four hint-driven fields a `Notify` call can carry.
///
/// `urgency`: byte `0` → [`Urgency::Low`], `2` → [`Urgency::Critical`],
/// anything else (byte `1`, hint absent, or the `urgency` key present with
/// a non-byte [`HintValue`]) → [`Urgency::Normal`] — the spec's own
/// documented value for "unspecified", so a malformed hint degrading to it
/// is exactly as safe as a genuinely absent one, never an error.
///
/// `transient`/`resident`: `true` only when the hint is present *and*
/// [`HintValue::Bool(true)`] — anything else (absent, or present with a
/// non-bool value) is `false`. Both hints default to "off" per spec, so
/// there is no separate "malformed" case to reason about here the way
/// urgency has one.
///
/// `image`: see [`resolve_image`] for the full six-source lookup chain.
pub fn parse_hints(
    hints: &HashMap<String, HintValue>,
    app_icon: &str,
    icon_tile: f32,
) -> ParsedHints {
    let urgency = match hints.get("urgency") {
        Some(HintValue::Byte(0)) => Urgency::Low,
        Some(HintValue::Byte(2)) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    let transient = matches!(hints.get("transient"), Some(HintValue::Bool(true)));
    let resident = matches!(hints.get("resident"), Some(HintValue::Bool(true)));
    let image = resolve_image(hints, app_icon, icon_tile);

    ParsedHints {
        urgency,
        transient,
        resident,
        image,
    }
}

/// The six-source icon lookup chain PLAN.md Stage 4 specifies, in order:
/// `image-data` → `image_data` → `image-path` → `image_path` → `app_icon`
/// (the `Notify` argument, not a hint) → legacy `icon_data`.
///
/// **Design decision: each source falls through to the next on either
/// absence *or* decode failure**, stopping at the first source that
/// actually produces a usable image. PLAN.md's own wording ("decode
/// failure → `None`, never an error") only pins down what a *single*
/// source's decode failure returns, not whether the overall lookup gives up
/// there or keeps trying older aliases — and giving up on the first present
/// alias would make the five fallback aliases pointless: a client sending a
/// well-formed `image-path` alongside a garbled `image-data` (both aliases
/// present, one usable) should still get an icon. Every real notification
/// daemon's own alias handling works the same way. If a source is absent,
/// or present but its bytes don't decode to anything, this function simply
/// moves on; only exhausting all six sources returns `None` (the themed
/// fallback tile is the UI's job from there, per PLAN.md).
fn resolve_image(
    hints: &HashMap<String, HintValue>,
    app_icon: &str,
    icon_tile: f32,
) -> Option<image::Handle> {
    for key in ["image-data", "image_data"] {
        if let Some(handle) = try_image_data(hints.get(key), icon_tile) {
            return Some(handle);
        }
    }
    for key in ["image-path", "image_path"] {
        if let Some(handle) = try_image_path(hints.get(key), icon_tile) {
            return Some(handle);
        }
    }
    if !app_icon.is_empty()
        && let Some(handle) = decode_path_str(app_icon, icon_tile)
    {
        return Some(handle);
    }
    if let Some(handle) = try_image_data(hints.get("icon_data"), icon_tile) {
        return Some(handle);
    }
    None
}

fn try_image_data(value: Option<&HintValue>, icon_tile: f32) -> Option<image::Handle> {
    match value {
        Some(HintValue::ImageData {
            width,
            height,
            rowstride,
            has_alpha,
            bits_per_sample,
            channels,
            data,
        }) => decode_image_data(
            *width,
            *height,
            *rowstride,
            *has_alpha,
            *bits_per_sample,
            *channels,
            data,
            icon_tile,
        ),
        _ => None,
    }
}

fn try_image_path(value: Option<&HintValue>, icon_tile: f32) -> Option<image::Handle> {
    match value {
        Some(HintValue::Str(path)) => decode_path_str(path, icon_tile),
        _ => None,
    }
}

/// Decodes the freedesktop `iiibiiay` "raw pixels" struct into a downsampled
/// RGBA [`image::Handle`], or `None` on any malformed input.
///
/// # Rowstride (teaching note)
///
/// A row of pixel data is not always exactly `width * channels` bytes —
/// encoders often pad each row to a 4-byte boundary for alignment, so
/// `rowstride` (bytes per row, as the sender measured it) can be larger
/// than the "tight" row width. Every row's start offset below is computed
/// as `y * rowstride`, **not** `y * width * channels` — using the wrong one
/// silently shears the image (each row reads a few bytes into the next
/// row's padding) rather than failing loudly, which is exactly the kind of
/// bug that only shows up as "this icon looks corrupted" days later. This
/// is why the freedesktop struct carries `rowstride` as its own field
/// instead of leaving callers to assume it.
///
/// `has_alpha` is accepted (it's part of the wire struct) but not consulted
/// for the actual byte layout — `channels` (`3` or `4`) already says
/// definitively whether a per-pixel alpha byte is present, and trusting the
/// field that actually determines the byte count is safer than trusting a
/// second field that's supposed to agree with it but isn't load-bearing for
/// decode. `bits_per_sample` must be exactly `8` (the only depth every
/// real-world sender uses); anything else is rejected rather than guessed
/// at.
///
/// Only `3` (RGB) and `4` (RGBA) channel counts are understood; anything
/// else, non-positive dimensions, or a `data` buffer too short for the
/// claimed `width`/`height`/`rowstride` all return `None` rather than
/// panicking on an out-of-bounds slice.
#[allow(clippy::too_many_arguments)]
fn decode_image_data(
    width: i32,
    height: i32,
    rowstride: i32,
    _has_alpha: bool,
    bits_per_sample: i32,
    channels: i32,
    data: &[u8],
    icon_tile: f32,
) -> Option<image::Handle> {
    if width <= 0 || height <= 0 || rowstride <= 0 || bits_per_sample != 8 {
        return None;
    }
    if channels != 3 && channels != 4 {
        return None;
    }

    let width = width as usize;
    let height = height as usize;
    let rowstride = rowstride as usize;
    let channels = channels as usize;

    if rowstride < width * channels {
        return None;
    }
    let last_row_start = height.checked_sub(1)?.checked_mul(rowstride)?;
    let needed = last_row_start.checked_add(width * channels)?;
    if data.len() < needed {
        return None;
    }

    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row_start = y * rowstride;
        for x in 0..width {
            let px_start = row_start + x * channels;
            let px = data.get(px_start..px_start + channels)?;
            match channels {
                3 => rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]),
                4 => rgba.extend_from_slice(&[px[0], px[1], px[2], px[3]]),
                _ => unreachable!("channels validated to be 3 or 4 above"),
            }
        }
    }

    Some(downsample_rgba(
        width,
        height,
        &rgba,
        icon_tile.round().max(1.0) as u32,
    ))
}

/// Resolves `path` against the "only absolute paths or `file://` URIs" rule
/// PLAN.md Stage 4 sets for `image-path`/`app_icon`, then decodes and
/// downsamples it.
///
/// **Known v0.1 limitation**: a bare themed-icon name (`"dialog-
/// information"`, no leading `/`, no `file://`) is not resolved against any
/// icon theme — it returns `None` (the themed fallback tile is the UI's
/// job). Freedesktop icon-theme lookup (`XDG_DATA_DIRS`, the icon theme
/// spec's fallback chain, SVG-vs-PNG preference) is real scope on its own
/// and is out of bounds for this stage; see the Stage 4 handoff.
///
/// `pub(crate)` since Stage 8: `modules::capture_bridge` decodes
/// `CaptureTaken`'s thumbnail with this exact function (PLAN.md Stage 8's
/// own words — "decode the png at `path` via the Stage 4 file-path
/// decoder") rather than re-deriving the same absolute-path-or-`file://`
/// rule a second time.
pub(crate) fn decode_path_str(path: &str, icon_tile: f32) -> Option<image::Handle> {
    let resolved = if let Some(rest) = path.strip_prefix("file://") {
        rest
    } else if path.starts_with('/') {
        path
    } else {
        return None;
    };

    // `::image` (leading `::`) reaches the external `image` crate at the
    // crate root, deliberately sidestepping this file's own `use
    // iced::widget::image;` binding of the plain name `image` — both are
    // legitimately called `image`, and only the crate-root path avoids the
    // clash. `default-features = false, features = ["png"]` (Cargo.toml's
    // own dated survey) means `open` can only ever successfully decode a
    // PNG; every other format's magic-byte match fails to find a compiled
    // decoder and `open` returns `Err`, which `.ok()?` turns into this
    // function's own `None` — never a panic, never a different error path.
    let decoded = ::image::open(resolved).ok()?.to_rgba8();
    let (width, height) = decoded.dimensions();
    Some(downsample_rgba(
        width as usize,
        height as usize,
        decoded.as_raw(),
        icon_tile.round().max(1.0) as u32,
    ))
}

/// Nearest-neighbor downsample of an already-tightly-packed RGBA buffer
/// (`width * height * 4` bytes, no padding — both [`decode_image_data`] and
/// the file-path decoder normalize to this shape first) to fit within
/// `max_dim` on its longer side, preserving aspect ratio.
///
/// Lifted from `saola-capture::modules::toast::thumbnail_handle`'s own
/// documented precedent (same algorithm, same "never upscale, never
/// distort by cropping to square" reasoning) — see that function's doc
/// comment for the full rationale. `Handle::from_rgba` only, per this
/// crate's sync-decode rule.
fn downsample_rgba(width: usize, height: usize, rgba: &[u8], max_dim: u32) -> image::Handle {
    let longest = (width.max(height) as u32).max(1);
    let scale = (max_dim as f32 / longest as f32).min(1.0);
    let dst_w = ((width as f32 * scale).round() as u32).max(1);
    let dst_h = ((height as f32 * scale).round() as u32).max(1);

    if dst_w as usize == width && dst_h as usize == height {
        return image::Handle::from_rgba(dst_w, dst_h, rgba.to_vec());
    }

    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for y in 0..dst_h {
        let src_y = (y * height as u32 / dst_h).min(height as u32 - 1);
        for x in 0..dst_w {
            let src_x = (x * width as u32 / dst_w).min(width as u32 - 1);
            let start = (src_y as usize * width + src_x as usize) * 4;
            match rgba.get(start..start + 4) {
                Some(px) => out.extend_from_slice(px),
                // Unreachable in practice (src_x/src_y are always in-bounds
                // by construction above), but the no-panic rule still wants
                // a value here rather than an indexing panic.
                None => out.extend_from_slice(&[0, 0, 0, 0xFF]),
            }
        }
    }
    image::Handle::from_rgba(dst_w, dst_h, out)
}

// ============================================================================
// Body markup stripping
// ============================================================================

/// Strips Pango/HTML markup from a notification body, and unescapes the
/// five XML entities the freedesktop spec's `body-markup` subset uses.
///
/// This crate's `GetCapabilities` deliberately omits `body-markup`
/// (`dbus.rs`'s own doc comment), but clients send markup regardless of
/// what capabilities a server advertises — `notify-send`'s own `--help`
/// documents `<b>`/`<i>`/`<u>`/`<a href="…">`/`<img src="…" alt="…"/>` as
/// always-on. Stripping at parse time means every downstream consumer
/// (toast card, history row) renders plain text unconditionally, with no
/// markup-vs-plain branch to get wrong later.
///
/// **Known limitation, by design (dependency-light over correct)**: this is
/// a byte-level tag stripper, not an XML parser. A bare `<` that is never
/// closed by a matching `>` swallows every character after it to the end of
/// the string, including one meant literally (a spec-compliant client
/// escapes a literal `<` as `&lt;`, so this only misbehaves on
/// already-non-compliant input). `<img>`'s `alt` text is dropped along with
/// the rest of the tag, not preserved as inline text — PLAN.md's own
/// instruction is "keep it dependency-light… beats pulling a parser crate",
/// and alt-text preservation isn't in the frozen contracts or the style
/// guide.
pub fn strip_markup(input: &str) -> String {
    let mut stripped = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => stripped.push(c),
        }
    }
    unescape_entities(&stripped)
}

/// Unescapes exactly the five XML entities the freedesktop markup subset
/// uses (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`). Anything else
/// starting with `&` (a numeric charref, an unrecognized named entity, or a
/// bare `&` that was never meant as an entity at all) is left untouched —
/// same "dependency-light over exhaustive" tradeoff as [`strip_markup`]
/// itself.
fn unescape_entities(input: &str) -> String {
    const ENTITIES: &[(&str, char)] = &[
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];

    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    'outer: while !rest.is_empty() {
        if rest.starts_with('&') {
            for (entity, replacement) in ENTITIES {
                if let Some(tail) = rest.strip_prefix(entity) {
                    out.push(*replacement);
                    rest = tail;
                    continue 'outer;
                }
            }
        }
        // No entity matched at this position — consume one char verbatim.
        // `rest` is non-empty by the loop guard, so `next()` always yields;
        // the `else break` keeps the resilience rule (no `expect` on a
        // runtime path) honest without a panic branch.
        let mut chars = rest.chars();
        let Some(c) = chars.next() else { break };
        out.push(c);
        rest = chars.as_str();
    }
    out
}

// ============================================================================
// DND policy
// ============================================================================

/// `effective_dnd = manual || recording` (AGENTS.md Architecture — verbatim).
pub fn effective_dnd(manual: bool, recording: bool) -> bool {
    manual || recording
}

/// Whether a notification should be kept off the toast stack (it still
/// lands in history either way — see [`Store::notify`]).
///
/// Recording auto-DND suppresses **everything**, including
/// [`Urgency::Critical`] — "no toast is ever burned into a screencast" is
/// unconditional (AGENTS.md's DND policy bullet), so this checks `recording
/// ` first and short-circuits before urgency is even consulted. Manual DND
/// suppresses everything **except** critical urgency when
/// `critical_bypasses_dnd` is configured on (the config's own
/// `critical-bypasses-dnd` knob, default `true` — see `config.rs`).
pub fn should_suppress_toast(
    urgency: Urgency,
    manual_dnd: bool,
    recording_dnd: bool,
    critical_bypasses_dnd: bool,
) -> bool {
    if recording_dnd {
        return true;
    }
    if manual_dnd {
        return !(urgency == Urgency::Critical && critical_bypasses_dnd);
    }
    false
}

// ============================================================================
// Expiry policy
// ============================================================================

/// What happens to a toast over time: either it never auto-dismisses, or it
/// does after a fixed span from when its stopwatch started running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryPolicy {
    Never,
    After(Duration),
}

/// Resolves a `Notify` call's `expire_timeout` (plus its urgency) into an
/// [`ExpiryPolicy`], per PLAN.md Stage 4's rules:
///
/// - [`Urgency::Critical`] or `expire_timeout == 0` → [`ExpiryPolicy::Never`]
///   — urgency is checked first, so a critical notification never expires
///   *regardless* of what `expire_timeout` says (a critical alert with an
///   explicit positive timeout still never auto-dismisses; the style guide
///   is unconditional here: "Urgent notifications have no life rule and
///   never auto-dismiss").
/// - `expire_timeout > 0` → [`ExpiryPolicy::After`] that many milliseconds,
///   replacing the theme's idle span.
/// - `expire_timeout == -1` (the freedesktop "server default" sentinel), or
///   any other negative value the spec doesn't define a meaning for → the
///   theme default, `toast_idle_ms`. Treating every negative value as
///   "default" rather than only `-1` is a deliberate defensive choice: a
///   client sending a nonsensical `-7` gets the same safe fallback as a
///   spec-compliant `-1`, not a policy this function has no defined answer
///   for.
pub fn expiry_policy(expire_timeout: i32, urgency: Urgency, toast_idle_ms: u32) -> ExpiryPolicy {
    if urgency == Urgency::Critical || expire_timeout == 0 {
        ExpiryPolicy::Never
    } else if expire_timeout > 0 {
        ExpiryPolicy::After(Duration::from_millis(expire_timeout as u64))
    } else {
        ExpiryPolicy::After(Duration::from_millis(u64::from(toast_idle_ms)))
    }
}

/// Whether `elapsed` (from a [`Stopwatch`]) has crossed `policy`'s life
/// span. [`ExpiryPolicy::Never`] never returns `true`, by construction.
pub fn has_expired(policy: ExpiryPolicy, elapsed: Duration) -> bool {
    match policy {
        ExpiryPolicy::Never => false,
        ExpiryPolicy::After(span) => elapsed >= span,
    }
}

/// Widens a *rest*-span policy into the toast's whole on-screen life by
/// adding the entrance and exit animations that bracket it
/// ([`Limits::toast_envelope_ms`]). [`ExpiryPolicy::Never`] widens to
/// itself — a card that never leaves has no envelope to add.
///
/// Style guide §5's default case works out to exactly its stated total:
/// `350 ms in + 5000 ms rest + 1000 ms out = 6.35 s`.
pub fn visible_policy(policy: ExpiryPolicy, envelope: Duration) -> ExpiryPolicy {
    match policy {
        ExpiryPolicy::Never => ExpiryPolicy::Never,
        ExpiryPolicy::After(rest) => ExpiryPolicy::After(rest.saturating_add(envelope)),
    }
}

/// A pausable stopwatch: "frozen total from every previous running
/// interval" (`elapsed_at_last_change`) plus "when the current interval
/// started, if it's running" (`resumed_at`). Lifted from
/// `saola-capture::modules::toast::Toast`'s own identically-shaped private
/// fields (that module's doc comment has the full rationale for this exact
/// shape over a plain "time remaining" countdown) — the toast surface's
/// hover-pause behavior (style guide §5: "Hover pauses both [the
/// auto-dismiss and the life rule]") needs a clock that can stop and later
/// resume without losing the time it already spent running, and a countdown
/// timer that's simply overwritten on pause/resume can't represent that
/// without its own extra bookkeeping.
///
/// Every method takes `now: Instant` as a parameter — this struct never
/// reads the clock itself, same rule as the rest of this file.
#[derive(Debug, Clone, Copy)]
pub struct Stopwatch {
    elapsed_at_last_change: Duration,
    resumed_at: Option<Instant>,
}

impl Stopwatch {
    /// A fresh stopwatch, already running as of `now`.
    pub fn started(now: Instant) -> Self {
        Self {
            elapsed_at_last_change: Duration::ZERO,
            resumed_at: Some(now),
        }
    }

    /// Total time this stopwatch has spent running, as of `now`: the frozen
    /// total from every previous interval, plus however long the current
    /// interval (if any) has run.
    pub fn elapsed(&self, now: Instant) -> Duration {
        match self.resumed_at {
            Some(started) => self.elapsed_at_last_change + now.saturating_duration_since(started),
            None => self.elapsed_at_last_change,
        }
    }

    /// Freezes the current interval's time into the running total and stops
    /// the clock. A no-op if already paused.
    pub fn pause(&mut self, now: Instant) {
        if let Some(started) = self.resumed_at.take() {
            self.elapsed_at_last_change += now.saturating_duration_since(started);
        }
    }

    /// Starts a new interval from `now`. A no-op if already running.
    pub fn resume(&mut self, now: Instant) {
        if self.resumed_at.is_none() {
            self.resumed_at = Some(now);
        }
    }

    /// Resets to a fresh, running-as-of-`now` stopwatch — used when a
    /// replace resets a toast's clock (style guide §6).
    pub fn reset(&mut self, now: Instant) {
        *self = Self::started(now);
    }
}

// ============================================================================
// Store — the toast stack and history.
// ============================================================================

/// One toast card on screen: the notification content plus its own
/// independent pausable clock.
#[derive(Debug, Clone)]
pub struct ToastEntry {
    pub notification: Notification,
    pub stopwatch: Stopwatch,
}

/// What [`Store::notify`] actually did — Stage 5 uses this to decide
/// whether a surface respawn is needed and whether to log/trace the
/// distinction; the state change itself has already happened by the time
/// this is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEffect {
    /// A brand-new toast was pushed (evicting the oldest if the stack was
    /// already at `toast_max_stack`), and a new history entry was appended.
    NewToast,
    /// `replaces_id` matched an id already tracked by this store: replaced
    /// in place wherever it was found (toast stopwatch reset; history entry
    /// overwritten, not duplicated). If `replaces_id` didn't match any
    /// currently on-screen toast, this still pushes a fresh toast (subject
    /// to the same eviction rule as `NewToast`) — see [`Store::notify`]'s
    /// doc comment for why.
    Replaced,
    /// Style guide §6: a *different*, fresh id (`replaces_id == 0`) arrived
    /// from an app that already has a toast on screen. That toast's card is
    /// replaced in place and its clock reset, but — unlike `Replaced` — a
    /// **new** history entry is appended, because this is genuinely a
    /// second, distinct notification, not an update to the first one.
    SameAppReplacedToast,
    /// DND suppressed this notification: no toast was touched at all, but
    /// it still landed in history (following the same replace-or-append
    /// rule as any other notification).
    SuppressedByDnd,
}

/// The in-memory notification store: the toast stack, the capped history
/// list, and which app groups are collapsed in the centre view.
///
/// **v0.1 has no persistence** (AGENTS.md Boundaries) — this is the whole
/// of the state; it lives exactly as long as the process does.
#[derive(Debug, Default)]
pub struct Store {
    toasts: Vec<ToastEntry>,
    history: Vec<Notification>,
    collapsed: HashSet<String>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn toasts(&self) -> &[ToastEntry] {
        &self.toasts
    }

    /// The capped history list, oldest first (same order notifications
    /// arrived in) — Stage 7's centre view groups this by `app_name` **at
    /// view time** (PLAN.md Stage 4: "grouped by app_name at view time"),
    /// not here; this store deliberately stays flat.
    pub fn history(&self) -> &[Notification] {
        &self.history
    }

    pub fn is_collapsed(&self, app_name: &str) -> bool {
        self.collapsed.contains(app_name)
    }

    /// Flips one app's group between collapsed and expanded in the centre
    /// view.
    pub fn toggle_collapsed(&mut self, app_name: &str) {
        if !self.collapsed.remove(app_name) {
            self.collapsed.insert(app_name.to_string());
        }
    }

    /// Applies one `Notify` call's worth of parsed content to the store —
    /// the single entry point for every replace/suppress/append rule
    /// PLAN.md Stage 4 specifies. `suppress_toast` is the caller's own
    /// [`should_suppress_toast`] result (this function doesn't recompute
    /// DND state; it isn't told `manual`/`recording`/config at all, only
    /// the yes/no answer, which keeps this function's own branching about
    /// replace-vs-same-app, not DND).
    ///
    /// # Priority order (highest first)
    ///
    /// 1. **Suppressed** — never touches the toast stack. History still
    ///    follows the `replaces_id` rule below (an app can still "update"
    ///    its own suppressed notification while DND is on).
    /// 2. **Explicit replace** (`replaces_id != 0`) — if a toast with that
    ///    id is currently on screen, its content is replaced in place and
    ///    its stopwatch reset ([`NotifyEffect::Replaced`]). If no toast
    ///    with that id is currently on screen (already expired/dismissed,
    ///    or never shown), this pushes a fresh toast instead — an app
    ///    reusing an id to send new content is still asking for that
    ///    content to be seen, and there's nothing on screen left to update
    ///    in place. Either way, history is replaced in place if that id is
    ///    still present there, or appended if not (still `Replaced`).
    /// 3. **Same app already on screen** (`replaces_id == 0`, style guide
    ///    §6) — the existing toast card is replaced in place and its clock
    ///    reset, but history gets a genuinely new entry
    ///    ([`NotifyEffect::SameAppReplacedToast`]).
    /// 4. **Otherwise** — a brand-new toast is pushed (evicting the oldest
    ///    if already at `toast_max_stack`) and a new history entry appended
    ///    ([`NotifyEffect::NewToast`]).
    pub fn notify(
        &mut self,
        notification: Notification,
        replaces_id: u32,
        suppress_toast: bool,
        now: Instant,
        limits: &Limits,
    ) -> NotifyEffect {
        if suppress_toast {
            self.upsert_history(notification, replaces_id, limits.history_cap);
            return NotifyEffect::SuppressedByDnd;
        }

        if replaces_id != 0 {
            if let Some(toast) = self
                .toasts
                .iter_mut()
                .find(|t| t.notification.id == replaces_id)
            {
                toast.notification = notification.clone();
                toast.stopwatch.reset(now);
            } else {
                self.push_toast(notification.clone(), limits.toast_max_stack, now);
            }
            self.upsert_history(notification, replaces_id, limits.history_cap);
            return NotifyEffect::Replaced;
        }

        if let Some(toast) = self
            .toasts
            .iter_mut()
            .find(|t| t.notification.app_name == notification.app_name)
        {
            toast.notification = notification.clone();
            toast.stopwatch.reset(now);
            self.push_history(notification, limits.history_cap);
            return NotifyEffect::SameAppReplacedToast;
        }

        self.push_toast(notification.clone(), limits.toast_max_stack, now);
        self.push_history(notification, limits.history_cap);
        NotifyEffect::NewToast
    }

    /// Removes one toast by id (a click-dismiss or a `Dismiss(id)` control
    /// call) — the caller is responsible for emitting
    /// `NotificationClosed(id, 2)` afterward (Stage 5's job; this file
    /// never touches D-Bus). Returns whether a toast was actually found and
    /// removed, so a caller can skip emitting the signal for an id that was
    /// already gone.
    pub fn dismiss_toast(&mut self, id: u32) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.notification.id != id);
        self.toasts.len() != before
    }

    /// Removes one notification **everywhere** — the history list and, if it
    /// is still on screen, the toast stack — as a dismissal from the
    /// notification centre (Stage 7). Returns whether anything was actually
    /// removed, so the caller can skip emitting
    /// `NotificationClosed(id, 2)` for an id that had already gone.
    ///
    /// Distinct from [`Self::dismiss_toast`], which only takes a card off the
    /// screen and deliberately leaves history alone: dismissing a *toast*
    /// means "stop showing me this now", dismissing from the *centre* means
    /// "I am done with this notification".
    pub fn dismiss_notification(&mut self, id: u32) -> bool {
        let toast_removed = self.dismiss_toast(id);
        let before = self.history.len();
        self.history.retain(|n| n.id != id);
        toast_removed || self.history.len() != before
    }

    /// Clears history **and** the toast stack (the centre's clear-all row),
    /// returning every id removed so the caller can emit
    /// `NotificationClosed(id, 2)` for each.
    ///
    /// The two lists are unioned rather than concatenated: a notification
    /// that is both on screen and in history must be reported once, and a
    /// card whose history entry has already aged out past `history_cap` must
    /// still be reported at all.
    pub fn clear_all(&mut self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.history.iter().map(|n| n.id).collect();
        for toast in &self.toasts {
            if !ids.contains(&toast.notification.id) {
                ids.push(toast.notification.id);
            }
        }
        self.history.clear();
        self.toasts.clear();
        ids
    }

    /// Removes every toast whose whole on-screen life ([`visible_policy`] —
    /// the [`ExpiryPolicy`] rest span plus the entrance/exit animation
    /// bracket) has elapsed as of `now`, returning their ids so the caller
    /// can emit `NotificationClosed(id, 1)` for each. History is untouched —
    /// expiry only ever affects the toast stack.
    ///
    /// A **paused** toast (hovered — style guide §5) can never expire here,
    /// with no branch of its own: [`Stopwatch::elapsed`] simply stops
    /// advancing while paused, so the comparison below stops moving too.
    pub fn expire_toasts(&mut self, now: Instant, limits: &Limits) -> Vec<u32> {
        let envelope = Duration::from_millis(u64::from(limits.toast_envelope_ms));
        let mut expired = Vec::new();
        self.toasts.retain(|toast| {
            let policy = expiry_policy(
                toast.notification.expire_timeout,
                toast.notification.urgency,
                limits.toast_idle_ms,
            );
            let life = visible_policy(policy, envelope);
            if has_expired(life, toast.stopwatch.elapsed(now)) {
                expired.push(toast.notification.id);
                false
            } else {
                true
            }
        });
        expired
    }

    /// Pauses one toast's stopwatch (hover-enter). A no-op if `id` isn't
    /// currently on screen.
    pub fn pause_toast(&mut self, id: u32, now: Instant) {
        if let Some(toast) = self.toasts.iter_mut().find(|t| t.notification.id == id) {
            toast.stopwatch.pause(now);
        }
    }

    /// Resumes one toast's stopwatch (hover-leave). A no-op if `id` isn't
    /// currently on screen.
    pub fn resume_toast(&mut self, id: u32, now: Instant) {
        if let Some(toast) = self.toasts.iter_mut().find(|t| t.notification.id == id) {
            toast.stopwatch.resume(now);
        }
    }

    /// Pushes a new toast, evicting the oldest first if already at
    /// `max_stack` (style guide §6: "stack at most three; the fourth
    /// replaces the oldest"). `max_stack == 0` is a degenerate but
    /// reachable config/theme value — it means "no toasts, ever," handled
    /// here rather than left to panic on an unreachable index.
    fn push_toast(&mut self, notification: Notification, max_stack: usize, now: Instant) {
        if max_stack == 0 {
            return;
        }
        if self.toasts.len() >= max_stack {
            self.toasts.remove(0);
        }
        self.toasts.push(ToastEntry {
            notification,
            stopwatch: Stopwatch::started(now),
        });
    }

    /// Appends a new history entry, dropping the oldest first once already
    /// at `cap`. `cap == 0` means history is disabled entirely.
    fn push_history(&mut self, notification: Notification, cap: usize) {
        if cap == 0 {
            self.history.clear();
            return;
        }
        self.history.push(notification);
        while self.history.len() > cap {
            self.history.remove(0);
        }
    }

    /// History's half of the explicit-`replaces_id` rule: overwrite in
    /// place if that id is still present in history, otherwise append as a
    /// new entry (still respecting the cap).
    fn upsert_history(&mut self, notification: Notification, replaces_id: u32, cap: usize) {
        if replaces_id != 0
            && let Some(existing) = self.history.iter_mut().find(|n| n.id == replaces_id)
        {
            *existing = notification;
            return;
        }
        self.push_history(notification, cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    fn limits() -> Limits {
        Limits {
            icon_tile: 36.0,
            toast_idle_ms: 5000,
            // Zero, so every pre-existing test in this file keeps asserting
            // about the *rest* span alone; the two tests that care about the
            // entrance/exit bracket set it explicitly.
            toast_envelope_ms: 0,
            toast_max_stack: 3,
            history_cap: 100,
        }
    }

    fn notification(id: u32, app_name: &str, now: Instant) -> Notification {
        Notification {
            id,
            app_name: app_name.to_string(),
            app_icon: String::new(),
            summary: "Summary".to_string(),
            body: "Body".to_string(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            image: None,
            expire_timeout: -1,
            transient: false,
            resident: false,
            posted_at: now,
        }
    }

    fn notification_with_urgency(
        id: u32,
        app_name: &str,
        urgency: Urgency,
        now: Instant,
    ) -> Notification {
        Notification {
            urgency,
            ..notification(id, app_name, now)
        }
    }

    // ------------------------------------------------------------------
    // parse_hints — urgency
    // ------------------------------------------------------------------

    #[test]
    fn urgency_byte_0_is_low() {
        let hints = HashMap::from([("urgency".to_string(), HintValue::Byte(0))]);
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Low);
    }

    #[test]
    fn urgency_byte_1_is_normal() {
        let hints = HashMap::from([("urgency".to_string(), HintValue::Byte(1))]);
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Normal);
    }

    #[test]
    fn urgency_byte_2_is_critical() {
        let hints = HashMap::from([("urgency".to_string(), HintValue::Byte(2))]);
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Critical);
    }

    #[test]
    fn urgency_missing_hint_is_normal() {
        let hints = HashMap::new();
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Normal);
    }

    #[test]
    fn urgency_wrong_type_hint_is_normal() {
        // A client that sent "urgency" as a bool instead of a byte — this
        // must degrade to Normal, never panic on the mismatch.
        let hints = HashMap::from([("urgency".to_string(), HintValue::Bool(true))]);
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Normal);
    }

    #[test]
    fn urgency_out_of_range_byte_is_normal() {
        let hints = HashMap::from([("urgency".to_string(), HintValue::Byte(200))]);
        assert_eq!(parse_hints(&hints, "", 36.0).urgency, Urgency::Normal);
    }

    // ------------------------------------------------------------------
    // parse_hints — transient / resident
    // ------------------------------------------------------------------

    #[test]
    fn transient_true_hint_sets_transient() {
        let hints = HashMap::from([("transient".to_string(), HintValue::Bool(true))]);
        assert!(parse_hints(&hints, "", 36.0).transient);
    }

    #[test]
    fn transient_false_hint_is_not_transient() {
        let hints = HashMap::from([("transient".to_string(), HintValue::Bool(false))]);
        assert!(!parse_hints(&hints, "", 36.0).transient);
    }

    #[test]
    fn transient_absent_defaults_false() {
        let hints = HashMap::new();
        assert!(!parse_hints(&hints, "", 36.0).transient);
    }

    #[test]
    fn transient_wrong_type_defaults_false() {
        let hints = HashMap::from([("transient".to_string(), HintValue::Byte(1))]);
        assert!(!parse_hints(&hints, "", 36.0).transient);
    }

    #[test]
    fn resident_true_hint_sets_resident() {
        let hints = HashMap::from([("resident".to_string(), HintValue::Bool(true))]);
        assert!(parse_hints(&hints, "", 36.0).resident);
    }

    #[test]
    fn resident_absent_defaults_false() {
        let hints = HashMap::new();
        assert!(!parse_hints(&hints, "", 36.0).resident);
    }

    // ------------------------------------------------------------------
    // decode_image_data — the iiibiiay struct and rowstride conversion
    // ------------------------------------------------------------------

    #[test]
    fn decode_rgb_three_channel_no_padding() {
        // 2x2, RGB, tight rows (rowstride == width * channels == 6).
        #[rustfmt::skip]
        let data = [
            255, 0, 0,    0, 255, 0,   // row 0
            0,   0, 255,  255, 255, 0, // row 1
        ];
        let handle =
            decode_image_data(2, 2, 6, false, 8, 3, &data, 36.0).expect("valid RGB data decodes");
        match handle {
            image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } => {
                assert_eq!((width, height), (2, 2));
                assert_eq!(
                    pixels.as_ref(),
                    &[
                        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255
                    ][..]
                );
            }
            other => panic!("expected Handle::Rgba, got {other:?}"),
        }
    }

    #[test]
    fn decode_rgba_four_channel_no_padding() {
        let data = [10, 20, 30, 40];
        let handle =
            decode_image_data(1, 1, 4, true, 8, 4, &data, 36.0).expect("valid RGBA data decodes");
        match handle {
            image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } => {
                assert_eq!((width, height), (1, 1));
                assert_eq!(pixels.as_ref(), &[10, 20, 30, 40][..]);
            }
            other => panic!("expected Handle::Rgba, got {other:?}"),
        }
    }

    #[test]
    fn decode_respects_rowstride_padding() {
        // 2x1 RGB rows, but each row is padded out to 8 bytes (2 bytes of
        // junk after the 6 real pixel bytes) — a rowstride wider than
        // width * channels, exactly the case the rowstride math exists for.
        #[rustfmt::skip]
        let data = [
            10, 20, 30,   40, 50, 60,   0xAA, 0xAA, // row 0 (6 real + 2 pad)
            70, 80, 90,   100, 110, 120, 0xAA, 0xAA, // row 1
        ];
        let handle =
            decode_image_data(2, 2, 8, false, 8, 3, &data, 36.0).expect("padded rows still decode");
        match handle {
            image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } => {
                assert_eq!((width, height), (2, 2));
                assert_eq!(
                    pixels.as_ref(),
                    &[
                        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255
                    ][..]
                );
            }
            other => panic!("expected Handle::Rgba, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_non_8_bit_samples() {
        let data = [0, 0, 0];
        assert!(decode_image_data(1, 1, 3, false, 1, 3, &data, 36.0).is_none());
    }

    #[test]
    fn decode_rejects_unsupported_channel_count() {
        let data = [0, 0];
        assert!(decode_image_data(1, 1, 2, false, 8, 2, &data, 36.0).is_none());
    }

    #[test]
    fn decode_rejects_non_positive_dimensions() {
        assert!(decode_image_data(0, 1, 3, false, 8, 3, &[], 36.0).is_none());
        assert!(decode_image_data(1, 0, 3, false, 8, 3, &[], 36.0).is_none());
        assert!(decode_image_data(-1, 1, 3, false, 8, 3, &[], 36.0).is_none());
    }

    #[test]
    fn decode_rejects_data_too_short() {
        // Claims 2x2 RGB (needs 12 bytes) but only provides 5.
        let data = [1, 2, 3, 4, 5];
        assert!(decode_image_data(2, 2, 6, false, 8, 3, &data, 36.0).is_none());
    }

    #[test]
    fn decode_downsamples_to_icon_tile_bound() {
        // 100x50 solid-color RGB — downsampling a constant-color image
        // makes the expected output trivial to state regardless of exactly
        // which source pixel nearest-neighbor picks.
        let mut data = Vec::with_capacity(100 * 50 * 3);
        for _ in 0..(100 * 50) {
            data.extend_from_slice(&[200, 100, 50]);
        }
        let handle = decode_image_data(100, 50, 300, false, 8, 3, &data, 36.0).expect("decodes");
        match handle {
            image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } => {
                // Longest side (100) scales to 36; the shorter side (50)
                // scales by the same factor: 50 * 0.36 = 18.
                assert_eq!((width, height), (36, 18));
                assert!(pixels.chunks_exact(4).all(|px| px == [200, 100, 50, 255]));
            }
            other => panic!("expected Handle::Rgba, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Image hint aliasing and precedence (resolve_image, via parse_hints)
    // ------------------------------------------------------------------

    fn small_image_data_hint() -> HintValue {
        HintValue::ImageData {
            width: 1,
            height: 1,
            rowstride: 3,
            has_alpha: false,
            bits_per_sample: 8,
            channels: 3,
            data: vec![1, 2, 3],
        }
    }

    #[test]
    fn image_data_hint_is_used_when_present() {
        let hints = HashMap::from([("image-data".to_string(), small_image_data_hint())]);
        assert!(parse_hints(&hints, "", 36.0).image.is_some());
    }

    #[test]
    fn image_data_underscore_alias_is_used() {
        let hints = HashMap::from([("image_data".to_string(), small_image_data_hint())]);
        assert!(parse_hints(&hints, "", 36.0).image.is_some());
    }

    #[test]
    fn image_path_underscore_alias_is_used() {
        let png = write_temp_png("image-path-underscore-alias.png", 2, 2);
        let hints = HashMap::from([(
            "image_path".to_string(),
            HintValue::Str(png.to_string_lossy().to_string()),
        )]);
        let image = parse_hints(&hints, "", 36.0).image;
        let _ = std::fs::remove_file(&png);
        assert!(image.is_some());
    }

    #[test]
    fn icon_data_legacy_alias_is_used_as_last_resort() {
        let hints = HashMap::from([("icon_data".to_string(), small_image_data_hint())]);
        assert!(parse_hints(&hints, "", 36.0).image.is_some());
    }

    #[test]
    fn no_image_source_present_returns_none() {
        let hints = HashMap::new();
        assert!(parse_hints(&hints, "", 36.0).image.is_none());
    }

    #[test]
    fn bare_icon_name_in_app_icon_is_not_decoded() {
        // Known v0.1 limitation: no icon-theme lookup, so a themed icon
        // name (no leading '/', no `file://`) never resolves.
        let hints = HashMap::new();
        assert!(
            parse_hints(&hints, "dialog-information", 36.0)
                .image
                .is_none()
        );
    }

    #[test]
    fn image_data_takes_precedence_over_image_path() {
        let png = write_temp_png("precedence-image-data-over-path.png", 4, 4);
        let hints = HashMap::from([
            ("image-data".to_string(), small_image_data_hint()),
            (
                "image-path".to_string(),
                HintValue::Str(png.to_string_lossy().to_string()),
            ),
        ]);
        let image = parse_hints(&hints, "", 36.0).image;
        let _ = std::fs::remove_file(&png);
        match image.expect("some image resolved") {
            image::Handle::Rgba { width, height, .. } => {
                // The 1x1 image-data hint wins, not the 4x4 file.
                assert_eq!((width, height), (1, 1));
            }
            other => panic!("expected Handle::Rgba, got {other:?}"),
        }
    }

    #[test]
    fn falls_through_to_next_source_when_earlier_source_fails_to_decode() {
        // image-data present but garbage (claims more data than provided);
        // image-path present and valid — the lookup must not give up at
        // the first present-but-broken source.
        let png = write_temp_png("fallthrough-after-garbage.png", 2, 2);
        let broken = HintValue::ImageData {
            width: 100,
            height: 100,
            rowstride: 300,
            has_alpha: false,
            bits_per_sample: 8,
            channels: 3,
            data: vec![1, 2, 3], // nowhere near enough for 100x100
        };
        let hints = HashMap::from([
            ("image-data".to_string(), broken),
            (
                "image-path".to_string(),
                HintValue::Str(png.to_string_lossy().to_string()),
            ),
        ]);
        let image = parse_hints(&hints, "", 36.0).image;
        let _ = std::fs::remove_file(&png);
        assert!(
            image.is_some(),
            "should have fallen through to the valid image-path"
        );
    }

    #[test]
    fn app_icon_argument_is_used_when_no_hints_present() {
        let png = write_temp_png("app-icon-argument.png", 2, 2);
        let hints = HashMap::new();
        let image = parse_hints(&hints, &png.to_string_lossy(), 36.0).image;
        let _ = std::fs::remove_file(&png);
        assert!(image.is_some());
    }

    #[test]
    fn file_uri_prefix_is_decoded() {
        let png = write_temp_png("file-uri-prefix.png", 2, 2);
        let uri = format!("file://{}", png.to_string_lossy());
        let hints = HashMap::from([("image-path".to_string(), HintValue::Str(uri))]);
        let image = parse_hints(&hints, "", 36.0).image;
        let _ = std::fs::remove_file(&png);
        assert!(image.is_some());
    }

    #[test]
    fn relative_path_in_image_path_is_not_decoded() {
        // Not absolute and not a file:// URI — unsupported per the "only
        // absolute paths or file:// URIs" rule, regardless of whether a
        // same-named file happens to exist in the working directory.
        let hints = HashMap::from([(
            "image-path".to_string(),
            HintValue::Str("relative/icon.png".to_string()),
        )]);
        assert!(parse_hints(&hints, "", 36.0).image.is_none());
    }

    /// Encodes a tiny solid-color PNG to a fresh path in the system temp
    /// directory and returns its path. `name` should be unique per test so
    /// parallel test threads never collide on the same file.
    fn write_temp_png(name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "saola-notifications-store-test-{}-{name}",
            std::process::id()
        ));
        let pixels = vec![0x40u8; (width * height * 4) as usize];
        let buffer =
            ::image::RgbaImage::from_raw(width, height, pixels).expect("valid buffer dims");
        buffer.save(&path).expect("temp PNG write succeeds");
        path
    }

    // ------------------------------------------------------------------
    // strip_markup
    // ------------------------------------------------------------------

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(strip_markup("plain text"), "plain text");
    }

    #[test]
    fn empty_string_stays_empty() {
        assert_eq!(strip_markup(""), "");
    }

    #[test]
    fn simple_tag_pair_is_stripped() {
        assert_eq!(strip_markup("<b>bold</b>"), "bold");
    }

    #[test]
    fn nested_tags_are_stripped() {
        assert_eq!(strip_markup("<b><i>text</i></b>"), "text");
    }

    #[test]
    fn self_closing_tag_is_stripped_entirely() {
        assert_eq!(
            strip_markup(r#"before<img src="x" alt="pic"/>after"#),
            "beforeafter"
        );
    }

    #[test]
    fn link_tag_keeps_inner_text_drops_attributes() {
        assert_eq!(
            strip_markup(r#"<a href="https://example.com">link</a>"#),
            "link"
        );
    }

    #[test]
    fn entities_are_unescaped() {
        assert_eq!(strip_markup("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(strip_markup("&lt;3"), "<3");
        assert_eq!(strip_markup("say &quot;hi&quot;"), "say \"hi\"");
        assert_eq!(strip_markup("it&apos;s"), "it's");
    }

    #[test]
    fn combined_tags_and_entities() {
        assert_eq!(strip_markup("<b>Tom &amp; Jerry</b>"), "Tom & Jerry");
    }

    #[test]
    fn unrecognized_entity_is_left_untouched() {
        assert_eq!(strip_markup("&nbsp;&unknown;"), "&nbsp;&unknown;");
    }

    #[test]
    fn unterminated_tag_consumes_to_end_of_string() {
        // Known limitation (documented on strip_markup): a bare, never-
        // closed '<' swallows the rest of the string.
        assert_eq!(strip_markup("before<never closes"), "before");
    }

    // ------------------------------------------------------------------
    // DND policy
    // ------------------------------------------------------------------

    #[test]
    fn effective_dnd_table() {
        assert!(!effective_dnd(false, false));
        assert!(effective_dnd(true, false));
        assert!(effective_dnd(false, true));
        assert!(effective_dnd(true, true));
    }

    #[test]
    fn should_suppress_toast_table() {
        // (urgency, manual, recording, critical_bypasses_dnd) -> expected suppress
        let urgencies = [Urgency::Low, Urgency::Normal, Urgency::Critical];
        let bools = [false, true];

        for &urgency in &urgencies {
            for &manual in &bools {
                for &recording in &bools {
                    for &bypass in &bools {
                        let expected = if recording {
                            true
                        } else if manual {
                            !(urgency == Urgency::Critical && bypass)
                        } else {
                            false
                        };
                        let actual = should_suppress_toast(urgency, manual, recording, bypass);
                        assert_eq!(
                            actual, expected,
                            "urgency={urgency:?} manual={manual} recording={recording} bypass={bypass}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn recording_dnd_suppresses_critical_even_with_bypass_enabled() {
        // The one case worth calling out by name, not just via the table
        // above: recording auto-DND is never bypassed, unlike manual DND.
        assert!(should_suppress_toast(Urgency::Critical, false, true, true));
    }

    #[test]
    fn manual_dnd_with_bypass_lets_critical_through() {
        assert!(!should_suppress_toast(Urgency::Critical, true, false, true));
    }

    #[test]
    fn manual_dnd_without_bypass_still_suppresses_critical() {
        assert!(should_suppress_toast(Urgency::Critical, true, false, false));
    }

    // ------------------------------------------------------------------
    // Actions (Stage 6)
    // ------------------------------------------------------------------

    fn action(key: &str, label: &str) -> Action {
        Action {
            key: key.to_string(),
            label: label.to_string(),
        }
    }

    #[test]
    fn default_action_is_none_when_there_are_no_actions() {
        let n = notification(1, "app", Instant::now());
        assert_eq!(default_action(&n), None);
    }

    #[test]
    fn default_action_is_none_when_only_non_default_actions_are_present() {
        let n = Notification {
            actions: vec![action("yes", "Yes"), action("no", "No")],
            ..notification(1, "app", Instant::now())
        };
        assert_eq!(default_action(&n), None);
    }

    #[test]
    fn default_action_finds_the_default_key_among_others() {
        let n = Notification {
            actions: vec![action("yes", "Yes"), action("default", "Open")],
            ..notification(1, "app", Instant::now())
        };
        assert_eq!(default_action(&n), Some(&action("default", "Open")));
    }

    #[test]
    fn action_pills_is_empty_when_there_are_no_actions() {
        let n = notification(1, "app", Instant::now());
        assert_eq!(action_pills(&n).count(), 0);
    }

    #[test]
    fn action_pills_excludes_only_the_default_key() {
        let n = Notification {
            actions: vec![
                action("default", "Open"),
                action("yes", "Yes"),
                action("no", "No"),
            ],
            ..notification(1, "app", Instant::now())
        };
        let pills: Vec<&Action> = action_pills(&n).collect();
        assert_eq!(pills, vec![&action("yes", "Yes"), &action("no", "No")]);
    }

    #[test]
    fn action_pills_preserves_notify_s_own_order() {
        let n = Notification {
            actions: vec![action("no", "No"), action("yes", "Yes")],
            ..notification(1, "app", Instant::now())
        };
        let pills: Vec<&Action> = action_pills(&n).collect();
        assert_eq!(pills, vec![&action("no", "No"), &action("yes", "Yes")]);
    }

    #[test]
    fn invoke_action_policy_closes_a_non_resident_toast() {
        assert_eq!(
            invoke_action_policy(false),
            ActionInvocation { close_after: true }
        );
    }

    #[test]
    fn invoke_action_policy_leaves_a_resident_toast_open() {
        assert_eq!(
            invoke_action_policy(true),
            ActionInvocation { close_after: false }
        );
    }

    // ------------------------------------------------------------------
    // Expiry policy
    // ------------------------------------------------------------------

    #[test]
    fn critical_urgency_never_expires_regardless_of_timeout() {
        assert_eq!(
            expiry_policy(5000, Urgency::Critical, 5000),
            ExpiryPolicy::Never
        );
        assert_eq!(
            expiry_policy(-1, Urgency::Critical, 5000),
            ExpiryPolicy::Never
        );
        assert_eq!(
            expiry_policy(0, Urgency::Critical, 5000),
            ExpiryPolicy::Never
        );
    }

    #[test]
    fn zero_timeout_never_expires_for_non_critical() {
        assert_eq!(expiry_policy(0, Urgency::Normal, 5000), ExpiryPolicy::Never);
        assert_eq!(expiry_policy(0, Urgency::Low, 5000), ExpiryPolicy::Never);
    }

    #[test]
    fn negative_one_timeout_uses_theme_default() {
        assert_eq!(
            expiry_policy(-1, Urgency::Normal, 5000),
            ExpiryPolicy::After(Duration::from_millis(5000))
        );
    }

    #[test]
    fn other_negative_timeout_also_uses_theme_default() {
        // Defensive: the spec only defines -1, but any negative value gets
        // the same safe fallback rather than an undefined policy.
        assert_eq!(
            expiry_policy(-7, Urgency::Normal, 5000),
            ExpiryPolicy::After(Duration::from_millis(5000))
        );
    }

    #[test]
    fn positive_timeout_replaces_idle_span() {
        assert_eq!(
            expiry_policy(9000, Urgency::Normal, 5000),
            ExpiryPolicy::After(Duration::from_millis(9000))
        );
    }

    #[test]
    fn has_expired_is_false_before_the_span() {
        assert!(!has_expired(
            ExpiryPolicy::After(Duration::from_secs(5)),
            Duration::from_secs(4)
        ));
    }

    #[test]
    fn has_expired_is_true_at_or_after_the_span() {
        assert!(has_expired(
            ExpiryPolicy::After(Duration::from_secs(5)),
            Duration::from_secs(5)
        ));
        assert!(has_expired(
            ExpiryPolicy::After(Duration::from_secs(5)),
            Duration::from_secs(6)
        ));
    }

    #[test]
    fn never_policy_never_expires() {
        assert!(!has_expired(
            ExpiryPolicy::Never,
            Duration::from_secs(u64::MAX / 2)
        ));
    }

    // ------------------------------------------------------------------
    // Stopwatch
    // ------------------------------------------------------------------

    #[test]
    fn stopwatch_elapsed_advances_while_running() {
        let start = Instant::now();
        let sw = Stopwatch::started(start);
        assert_eq!(
            sw.elapsed(start + Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn stopwatch_pause_freezes_elapsed() {
        let start = Instant::now();
        let mut sw = Stopwatch::started(start);
        let paused_at = start + Duration::from_secs(2);
        sw.pause(paused_at);
        // Time passes in the real world, but the stopwatch shouldn't move.
        assert_eq!(
            sw.elapsed(paused_at + Duration::from_secs(10)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn stopwatch_resume_continues_from_frozen_total() {
        let start = Instant::now();
        let mut sw = Stopwatch::started(start);
        sw.pause(start + Duration::from_secs(2));
        sw.resume(start + Duration::from_secs(5)); // 3s paused, doesn't count
        assert_eq!(
            sw.elapsed(start + Duration::from_secs(6)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn stopwatch_double_pause_is_a_no_op() {
        let start = Instant::now();
        let mut sw = Stopwatch::started(start);
        sw.pause(start + Duration::from_secs(2));
        sw.pause(start + Duration::from_secs(100)); // already paused — must not extend the total
        assert_eq!(
            sw.elapsed(start + Duration::from_secs(200)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn stopwatch_double_resume_is_a_no_op() {
        let start = Instant::now();
        let mut sw = Stopwatch::started(start);
        sw.resume(start + Duration::from_secs(50)); // already running — must not reset the start point
        assert_eq!(
            sw.elapsed(start + Duration::from_secs(3)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn stopwatch_reset_restarts_from_zero() {
        let start = Instant::now();
        let mut sw = Stopwatch::started(start);
        let later = start + Duration::from_secs(4);
        sw.reset(later);
        assert_eq!(
            sw.elapsed(later + Duration::from_secs(1)),
            Duration::from_secs(1)
        );
    }

    // ------------------------------------------------------------------
    // CloseReason
    // ------------------------------------------------------------------

    #[test]
    fn close_reason_wire_values_match_the_frozen_contract() {
        assert_eq!(CloseReason::Expired.as_u32(), 1);
        assert_eq!(CloseReason::UserDismissed.as_u32(), 2);
        assert_eq!(CloseReason::CloseNotification.as_u32(), 3);
    }

    // ------------------------------------------------------------------
    // Store — replace-vs-same-app semantics
    // ------------------------------------------------------------------

    #[test]
    fn fresh_notification_pushes_toast_and_history() {
        let now = Instant::now();
        let mut store = Store::new();
        let effect = store.notify(notification(1, "app-a", now), 0, false, now, &limits());
        assert_eq!(effect, NotifyEffect::NewToast);
        assert_eq!(store.toasts().len(), 1);
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.toasts()[0].notification.id, 1);
    }

    #[test]
    fn fourth_toast_evicts_the_oldest() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits(); // toast_max_stack: 3
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        store.notify(notification(2, "app-b", now), 0, false, now, &limits);
        store.notify(notification(3, "app-c", now), 0, false, now, &limits);
        store.notify(notification(4, "app-d", now), 0, false, now, &limits);

        let ids: Vec<u32> = store.toasts().iter().map(|t| t.notification.id).collect();
        assert_eq!(ids, vec![2, 3, 4], "oldest (id 1) should have been evicted");
        // History keeps all four — only the toast stack is capped.
        assert_eq!(store.history().len(), 4);
    }

    #[test]
    fn explicit_replace_of_an_onscreen_toast_replaces_in_place_and_resets_stopwatch() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);

        let later = now + Duration::from_secs(3);
        let mut replacement = notification(1, "app-a", later);
        replacement.summary = "Updated".to_string();
        let effect = store.notify(replacement, 1, false, later, &limits);

        assert_eq!(effect, NotifyEffect::Replaced);
        assert_eq!(
            store.toasts().len(),
            1,
            "replace must not add a second toast"
        );
        assert_eq!(store.toasts()[0].notification.summary, "Updated");
        // Stopwatch reset: elapsed from `later` should read ~0, not ~3s.
        assert_eq!(store.toasts()[0].stopwatch.elapsed(later), Duration::ZERO);
        // History replaced in place, not duplicated.
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.history()[0].summary, "Updated");
    }

    #[test]
    fn explicit_replace_of_an_id_not_currently_onscreen_pushes_a_fresh_toast() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        // id 1 was shown and already dismissed/expired — gone from toasts,
        // but still in history.
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        store.dismiss_toast(1);
        assert!(store.toasts().is_empty());
        assert_eq!(store.history().len(), 1);

        let later = now + Duration::from_secs(1);
        let mut replacement = notification(1, "app-a", later);
        replacement.summary = "Second wind".to_string();
        let effect = store.notify(replacement, 1, false, later, &limits);

        assert_eq!(effect, NotifyEffect::Replaced);
        assert_eq!(
            store.toasts().len(),
            1,
            "should push a fresh toast since none was on screen"
        );
        assert_eq!(store.toasts()[0].notification.summary, "Second wind");
        // History entry for id 1 replaced in place, not duplicated.
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.history()[0].summary, "Second wind");
    }

    #[test]
    fn same_app_new_id_replaces_toast_card_but_appends_new_history_entry() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "slack", now), 0, false, now, &limits);

        let later = now + Duration::from_secs(2);
        let effect = store.notify(notification(2, "slack", later), 0, false, later, &limits);

        assert_eq!(effect, NotifyEffect::SameAppReplacedToast);
        // Still exactly one toast on screen — the second replaced the first
        // card in place, it didn't stack alongside it.
        assert_eq!(store.toasts().len(), 1);
        assert_eq!(store.toasts()[0].notification.id, 2);
        assert_eq!(store.toasts()[0].stopwatch.elapsed(later), Duration::ZERO);
        // But history has BOTH — two distinct notifications really arrived.
        assert_eq!(store.history().len(), 2);
        assert_eq!(store.history()[0].id, 1);
        assert_eq!(store.history()[1].id, 2);
    }

    #[test]
    fn different_app_new_id_does_not_trigger_same_app_replace() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "slack", now), 0, false, now, &limits);
        let effect = store.notify(notification(2, "discord", now), 0, false, now, &limits);

        assert_eq!(effect, NotifyEffect::NewToast);
        assert_eq!(store.toasts().len(), 2);
    }

    #[test]
    fn suppressed_notification_skips_toast_but_lands_in_history() {
        let now = Instant::now();
        let mut store = Store::new();
        let effect = store.notify(notification(1, "app-a", now), 0, true, now, &limits());

        assert_eq!(effect, NotifyEffect::SuppressedByDnd);
        assert!(store.toasts().is_empty());
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn suppressed_replace_updates_history_in_place_without_touching_toasts() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        assert_eq!(store.toasts().len(), 1);

        let later = now + Duration::from_secs(1);
        let mut replacement = notification(1, "app-a", later);
        replacement.summary = "Suppressed update".to_string();
        let effect = store.notify(replacement, 1, true, later, &limits);

        assert_eq!(effect, NotifyEffect::SuppressedByDnd);
        // The existing toast (shown before DND engaged) is left as-is —
        // still carrying the original notification's summary, "Summary"
        // (the `notification()` helper's fixed default), not the
        // suppressed replacement's.
        assert_eq!(store.toasts().len(), 1);
        assert_eq!(store.toasts()[0].notification.summary, "Summary");
        // History reflects the update.
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.history()[0].summary, "Suppressed update");
    }

    // ------------------------------------------------------------------
    // History cap
    // ------------------------------------------------------------------

    #[test]
    fn history_cap_drops_the_oldest_entry() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = Limits {
            history_cap: 2,
            ..limits()
        };
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        store.notify(notification(2, "app-b", now), 0, false, now, &limits);
        store.notify(notification(3, "app-c", now), 0, false, now, &limits);

        let ids: Vec<u32> = store.history().iter().map(|n| n.id).collect();
        assert_eq!(
            ids,
            vec![2, 3],
            "oldest entry (id 1) should have been dropped"
        );
    }

    #[test]
    fn history_cap_zero_keeps_history_empty() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = Limits {
            history_cap: 0,
            ..limits()
        };
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        assert!(store.history().is_empty());
    }

    #[test]
    fn toast_max_stack_zero_never_shows_a_toast() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = Limits {
            toast_max_stack: 0,
            ..limits()
        };
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        assert!(store.toasts().is_empty());
        // History is unaffected by the toast-stack limit.
        assert_eq!(store.history().len(), 1);
    }

    // ------------------------------------------------------------------
    // Dismiss / expire / pause-resume
    // ------------------------------------------------------------------

    #[test]
    fn dismiss_toast_removes_only_that_id() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        store.notify(notification(2, "app-b", now), 0, false, now, &limits);

        assert!(store.dismiss_toast(1));
        let ids: Vec<u32> = store.toasts().iter().map(|t| t.notification.id).collect();
        assert_eq!(ids, vec![2]);
        // Already gone — a second dismiss reports nothing happened.
        assert!(!store.dismiss_toast(1));
    }

    #[test]
    fn expire_toasts_removes_expired_and_returns_their_ids() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits(); // toast_idle_ms: 5000
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);

        let too_soon = now + Duration::from_millis(4999);
        assert!(store.expire_toasts(too_soon, &limits).is_empty());
        assert_eq!(store.toasts().len(), 1);

        let after = now + Duration::from_millis(5000);
        let expired = store.expire_toasts(after, &limits);
        assert_eq!(expired, vec![1]);
        assert!(store.toasts().is_empty());
    }

    #[test]
    fn a_toast_stays_on_screen_through_its_entrance_and_exit_animations() {
        let now = Instant::now();
        let mut store = Store::new();
        // The real theme's bracket: 350 ms slide-in + 1000 ms fade-out around
        // a 5000 ms rest span — style guide §5's 6.35 s total.
        let limits = Limits {
            toast_envelope_ms: 1350,
            ..limits()
        };
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);

        let rest_over = now + Duration::from_millis(5000);
        assert!(
            store.expire_toasts(rest_over, &limits).is_empty(),
            "the rest span ending is not the end of the card — the fade-out still has to play"
        );

        let still_fading = now + Duration::from_millis(6349);
        assert!(store.expire_toasts(still_fading, &limits).is_empty());

        let total = now + Duration::from_millis(6350);
        assert_eq!(store.expire_toasts(total, &limits), vec![1]);
        assert!(store.toasts().is_empty());
    }

    #[test]
    fn the_animation_envelope_never_expires_a_never_expiring_toast() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = Limits {
            toast_envelope_ms: 1350,
            ..limits()
        };
        store.notify(
            notification_with_urgency(1, "app-a", Urgency::Critical, now),
            0,
            false,
            now,
            &limits,
        );

        let far_future = now + Duration::from_secs(3600);
        assert!(store.expire_toasts(far_future, &limits).is_empty());
        assert_eq!(store.toasts().len(), 1);
    }

    #[test]
    fn expire_toasts_leaves_critical_untouched() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(
            notification_with_urgency(1, "app-a", Urgency::Critical, now),
            0,
            false,
            now,
            &limits,
        );

        let far_future = now + Duration::from_secs(3600);
        let expired = store.expire_toasts(far_future, &limits);
        assert!(expired.is_empty());
        assert_eq!(store.toasts().len(), 1);
    }

    #[test]
    fn pause_and_resume_toast_affect_only_that_id() {
        let now = Instant::now();
        let mut store = Store::new();
        let limits = limits();
        store.notify(notification(1, "app-a", now), 0, false, now, &limits);
        store.notify(notification(2, "app-b", now), 0, false, now, &limits);

        let paused_at = now + Duration::from_secs(1);
        store.pause_toast(1, paused_at);

        let later = now + Duration::from_secs(10);
        let elapsed_1 = store
            .toasts()
            .iter()
            .find(|t| t.notification.id == 1)
            .unwrap()
            .stopwatch
            .elapsed(later);
        let elapsed_2 = store
            .toasts()
            .iter()
            .find(|t| t.notification.id == 2)
            .unwrap()
            .stopwatch
            .elapsed(later);

        assert_eq!(
            elapsed_1,
            Duration::from_secs(1),
            "paused toast stays frozen"
        );
        assert_eq!(
            elapsed_2,
            Duration::from_secs(10),
            "untouched toast keeps running"
        );

        store.resume_toast(1, later);
        let after_resume = later + Duration::from_secs(2);
        let elapsed_1_after = store
            .toasts()
            .iter()
            .find(|t| t.notification.id == 1)
            .unwrap()
            .stopwatch
            .elapsed(after_resume);
        assert_eq!(
            elapsed_1_after,
            Duration::from_secs(3),
            "1s before pause + 2s after resume"
        );
    }

    // ------------------------------------------------------------------
    // Collapsed groups
    // ------------------------------------------------------------------

    #[test]
    fn toggle_collapsed_flips_state() {
        let mut store = Store::new();
        assert!(!store.is_collapsed("slack"));
        store.toggle_collapsed("slack");
        assert!(store.is_collapsed("slack"));
        store.toggle_collapsed("slack");
        assert!(!store.is_collapsed("slack"));
    }

    // ------------------------------------------------------------------
    // Centre dismissals (Stage 7)
    // ------------------------------------------------------------------

    #[test]
    fn dismissing_from_the_centre_removes_the_history_entry() {
        let now = Instant::now();
        let mut store = Store::new();
        store.notify(notification(1, "slack", now), 0, true, now, &limits());
        store.notify(notification(2, "mail", now), 0, true, now, &limits());

        assert!(store.dismiss_notification(1));
        assert_eq!(
            store.history().iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![2],
            "only the dismissed entry leaves history"
        );
    }

    #[test]
    fn dismissing_from_the_centre_also_takes_the_card_off_screen() {
        let now = Instant::now();
        let mut store = Store::new();
        store.notify(notification(1, "slack", now), 0, false, now, &limits());

        assert!(store.dismiss_notification(1));
        assert!(
            store.toasts().is_empty(),
            "a notification dismissed in the centre cannot still be a live toast"
        );
        assert!(store.history().is_empty());
    }

    #[test]
    fn dismissing_an_unknown_id_reports_nothing_was_removed() {
        let now = Instant::now();
        let mut store = Store::new();
        store.notify(notification(1, "slack", now), 0, true, now, &limits());

        assert!(
            !store.dismiss_notification(99),
            "an id in neither history nor the stack must not claim a removal — the caller \
             emits NotificationClosed off this answer"
        );
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn clear_all_empties_history_and_the_stack_and_reports_every_id() {
        let now = Instant::now();
        let mut store = Store::new();
        store.notify(notification(1, "slack", now), 0, false, now, &limits());
        store.notify(notification(2, "mail", now), 0, false, now, &limits());
        // Suppressed: in history, never on the toast stack.
        store.notify(notification(3, "cal", now), 0, true, now, &limits());

        let mut ids = store.clear_all();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(store.history().is_empty());
        assert!(store.toasts().is_empty());
    }

    #[test]
    fn clear_all_reports_a_live_toast_that_history_has_already_dropped() {
        let now = Instant::now();
        let mut store = Store::new();
        let tight = Limits {
            history_cap: 1,
            ..limits()
        };
        store.notify(notification(1, "slack", now), 0, false, now, &tight);
        store.notify(notification(2, "mail", now), 0, false, now, &tight);

        let mut ids = store.clear_all();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![1, 2],
            "id 1 aged out of a one-deep history but is still a card on screen, so clearing \
             still owes it a NotificationClosed"
        );
        assert!(store.toasts().is_empty());
    }

    #[test]
    fn clear_all_on_an_empty_store_reports_nothing() {
        let mut store = Store::new();
        assert!(store.clear_all().is_empty());
    }
}
