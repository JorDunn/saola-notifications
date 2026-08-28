//! The toast stack: style guide §6's notification card, animating §5's exact
//! timing.
//!
//! Lifted from `saola-capture::modules::toast` — the interim implementation
//! that repo wrote explicitly to be handed over here — and generalized from
//! its four hardcoded `ToastKind`s onto Stage 4's [`crate::store::Notification`].
//! Three things changed in the move, and each is worth knowing about:
//!
//! - **The state moved out.** Capture's `ToastStack` owned its cards and
//!   their stopwatches; here [`crate::store::Store`] owns both, because the
//!   notification centre (Stage 7) renders the same model. See
//!   `modules/mod.rs` for the two documented deviations from the panel's
//!   module pattern that follow from this.
//! - **The styles moved up.** Capture derived an `ink_card_style` locally
//!   because saola-theme had no opaque-ink card helper; v0.13 ships
//!   [`saola_theme::style::container::notification_card`],
//!   [`saola_theme::style::notification::life_rule`] and
//!   [`saola_theme::style::notification::icon_tile`], plus the
//!   `sizes.icon_tile` / `sizes.life_rule` tokens capture had to invent as
//!   local constants. Nothing in this file names a color, a size or a
//!   duration of its own.
//! - **The timing became per-notification.** Capture's every toast lived
//!   exactly `motion.toast_total`; a real `Notify` call carries its own
//!   `expire_timeout`, and a critical one never expires at all. See "The
//!   three-phase envelope" below.
//!
//! # Time is injected, never read (teaching note)
//!
//! Every `now` in this file is a parameter. `main.rs` reads the real clock
//! in exactly two places — when a `Notify` event arrives and on each
//! animation tick — and passes the `Instant` down. That is what lets every
//! function below be unit-tested against fabricated times rather than
//! `sleep`-ing through a six-second animation.
//!
//! # The three-phase envelope (teaching note)
//!
//! §5 fixes a toast's life at three phases: `motion.toast_in` sliding in
//! while fading in, then a rest span, then `motion.toast_out` fading out in
//! place. The **rest span** is what varies per notification —
//! [`crate::store::expiry_policy`] resolves it from `expire_timeout` and
//! urgency (`-1` → the theme's `motion.toast_idle`; a positive value
//! replaces it; `0` or `Urgency::Critical` → never) — and the entrance and
//! exit bracket it either side. So the card's total life is
//! `toast_in + rest + toast_out`, which for the theme's own default is
//! `350 + 5000 + 1000 = 6350 ms`, §5's stated total exactly.
//!
//! [`card_alpha`] and [`life_fraction`] are that envelope, generalized over
//! the rest span. saola-theme's own [`saola_theme::motion::toast_alpha`] and
//! [`saola_theme::motion::life_fraction`] encode the same shape but hardwire
//! `motion.toast_idle` as the rest span, so they cannot express a client's
//! `expire_timeout` — the gap is recorded in `docs/UPSTREAM-THEME-DEBT.md`.
//! Both functions here are built from `saola_theme::motion::fraction` and the
//! `motion.toast_*` tokens (never a local number), and the two
//! `matches_the_theme_*` tests below pin them to the theme's own answers for
//! the default span, so the local generalization can never drift from §5.
//!
//! # The slide-in, without a transform (teaching note)
//!
//! §5's "slide in from the right edge (`translateX(120% → 0)`)" has no iced
//! 0.14 equivalent — there is no subtree transform, and no subtree opacity
//! either. Capture's two workarounds carry over verbatim:
//!
//! - **Travel** is a *leading spacer* that shrinks to zero
//!   ([`slide_offset`]). The toast surface is declared exactly
//!   `sizes.notification_card_width` wide, and a layer-shell surface has no
//!   canvas past its own negotiated size, so a spacer one card wide pushes
//!   the card fully off the surface — indistinguishable from off-screen.
//!   (That is why 100% of the card width stands in for §5's 120%: anything
//!   past 100% is already invisible.)
//! - **Opacity** is per-color alpha scaling. `notification_card(t, alpha)`
//!   takes the card's own alpha; every color drawn *inside* it is scaled
//!   with [`saola_theme::ColorExt::with_opacity`] at the same alpha.

use std::time::{Duration, Instant};

use iced::widget::{Space, column, container, image, mouse_area, progress_bar, row, text};
use iced::{Center, Element, Length, Subscription};
use saola_theme::{ColorExt, Theme};

use crate::store::{ExpiryPolicy, Notification, Store, ToastEntry, Urgency, expiry_policy};

/// How many lines of body text a card reserves room for.
///
/// A layer-shell surface's height has to be declared *before* iced lays the
/// text out (see `main.rs`'s `sync_toast_surface`), so a card cannot hug a
/// body of unknown length: the height is fixed and the body clips. Two lines
/// is the compromise — it covers the overwhelming majority of real
/// notification bodies, and the notification centre (Stage 7) is where the
/// full text lives when it doesn't. §6 specifies the body's size and leading
/// but not a line count, so this is a local layout decision, not a spec
/// value.
const BODY_LINES: f32 = 2.0;

// ---------------------------------------------------------------------
// The three-phase envelope — pure math over the motion tokens.
// ---------------------------------------------------------------------

/// The entrance-plus-exit bracket around a toast's rest span, in one value:
/// `motion.toast_in + motion.toast_out`. Feeds
/// [`crate::store::Limits::toast_envelope_ms`], which is what keeps
/// [`crate::store::Store::expire_toasts`] from removing a card before its
/// fade-out has played.
pub fn envelope(theme: &Theme) -> Duration {
    Duration::from_millis(u64::from(theme.motion.toast_in) + u64::from(theme.motion.toast_out))
}

/// `Duration` → whole milliseconds, saturating, for the `u32`-millisecond
/// shape `saola_theme::motion::fraction` takes. A rest span long enough to
/// overflow a `u32` of milliseconds (49 days) is not a real notification,
/// but it is a reachable `expire_timeout`, so it clamps rather than wraps.
fn as_millis_u32(span: Duration) -> u32 {
    u32::try_from(span.as_millis()).unwrap_or(u32::MAX)
}

/// The rest span this notification's card sits still for — its urgency and
/// `expire_timeout` resolved against the theme's default
/// (`motion.toast_idle`). A thin wrapper over
/// [`crate::store::expiry_policy`] so no view code has to remember which
/// token is the default.
pub fn rest_policy(theme: &Theme, notification: &Notification) -> ExpiryPolicy {
    expiry_policy(
        notification.expire_timeout,
        notification.urgency,
        theme.motion.toast_idle,
    )
}

/// The card's opacity at `elapsed`: fade in over `motion.toast_in`, hold at
/// `1.0` for the rest span, fade out over `motion.toast_out`, then stay at
/// `0.0`.
///
/// [`ExpiryPolicy::Never`] (an urgent card, or an explicit
/// `expire_timeout` of `0`) fades in and then holds at `1.0` forever: §5's
/// "urgent notifications ... never auto-dismiss" is about the card leaving,
/// not about it arriving, so the entrance still plays.
pub fn card_alpha(theme: &Theme, policy: ExpiryPolicy, elapsed: Duration) -> f32 {
    let entrance = Duration::from_millis(u64::from(theme.motion.toast_in));

    if elapsed < entrance {
        return saola_theme::motion::fraction(elapsed, theme.motion.toast_in);
    }

    let ExpiryPolicy::After(rest) = policy else {
        // Arrived, and never leaving.
        return 1.0;
    };

    if elapsed < entrance + rest {
        return 1.0;
    }

    let fading = elapsed.saturating_sub(entrance + rest);
    1.0 - saola_theme::motion::fraction(fading, theme.motion.toast_out)
}

/// The life rule's remaining fraction at `elapsed`: full through the
/// entrance (nothing to count down yet), draining linearly to `0.0` across
/// the rest span, then empty through the fade-out.
///
/// `None` means **this card has no life rule at all** — §5: "urgent
/// notifications have no life rule and never auto-dismiss". A rule that
/// never moves would be a lie about a card that never leaves, so the caller
/// draws nothing rather than a full-width rule.
pub fn life_fraction(theme: &Theme, policy: ExpiryPolicy, elapsed: Duration) -> Option<f32> {
    let ExpiryPolicy::After(rest) = policy else {
        return None;
    };
    let entrance = Duration::from_millis(u64::from(theme.motion.toast_in));

    if elapsed < entrance {
        return Some(1.0);
    }
    if elapsed < entrance + rest {
        let resting = elapsed.saturating_sub(entrance);
        return Some(1.0 - saola_theme::motion::fraction(resting, as_millis_u32(rest)));
    }
    Some(0.0)
}

/// The leading spacer width standing in for §5's `translateX` — a full card
/// width at `elapsed == 0`, shrinking linearly to `0` at `motion.toast_in`.
/// See this module's doc comment for why a spacer, and why 100% rather than
/// §5's literal 120%.
pub fn slide_offset(theme: &Theme, elapsed: Duration) -> f32 {
    let travelled = saola_theme::motion::fraction(elapsed, theme.motion.toast_in);
    theme.sizes.notification_card_width * (1.0 - travelled)
}

// ---------------------------------------------------------------------
// Surface geometry — pure, and called before any surface exists.
// ---------------------------------------------------------------------

/// One card's declared height, in logical pixels: the icon tile (or the text
/// block, whichever is taller) inside `sizes.popover_padding` on every edge,
/// plus the `sizes.life_rule` strip across the bottom.
///
/// The rule's strip is counted even for a card that has no rule (see
/// [`life_fraction`]) so that a stack of mixed urgencies keeps one rhythm
/// and the surface arithmetic never has to ask about urgency; the urgent
/// card simply leaves that strip empty.
pub fn card_height(theme: &Theme, notification: &Notification) -> f32 {
    // Stage 6 ("Actions") adds an action-pill row to the card and will read
    // `notification.actions` here to size it. Nothing about the Stage 5 card
    // varies per notification, so the parameter is taken now — the signature
    // is what `main.rs` and (Stage 7) the centre call — rather than added
    // later and rippled through every call site.
    let _ = notification;

    let title = theme.typography.size.body * theme.typography.line_height;
    let body = theme.typography.size.secondary * theme.typography.line_height;
    let text_block = title + theme.sizes.gap_tight + body * BODY_LINES;
    let content = text_block.max(theme.sizes.icon_tile);

    theme.sizes.popover_padding * 2.0 + content + theme.sizes.life_rule
}

/// The toast surface's declared height for the whole stack: every card's
/// height plus one `sizes.island_gap` between each pair. Zero for an empty
/// stack — `main.rs` unmaps the surface entirely at that point rather than
/// mapping a zero-height one.
pub fn stack_height(theme: &Theme, toasts: &[ToastEntry]) -> u32 {
    if toasts.is_empty() {
        return 0;
    }
    let cards: f32 = toasts
        .iter()
        .map(|toast| card_height(theme, &toast.notification))
        .sum();
    let gaps = (toasts.len() - 1) as f32 * theme.sizes.island_gap;
    (cards + gaps).round() as u32
}

// ---------------------------------------------------------------------
// The module: state, messages, update, view, subscription.
// ---------------------------------------------------------------------

/// How often the stack redraws while at least one card is on screen.
///
/// AGENTS.md's "every module maps to a signal, never a poll" has exactly one
/// documented exception, and this is it: §5's motion cannot be expressed as
/// an event, so a card that is sliding, fading or draining its life rule has
/// to be repainted on a timer. The gate is what keeps it an animation rather
/// than a poll — [`Toasts::subscription`] returns `Subscription::none()`
/// whenever the stack is empty, which is almost all of a desktop's day.
///
/// `saola-theme` has no frame-cadence token to take this from (its `motion.*`
/// tokens are all design durations, not redraw intervals), so the value is
/// local; the gap is recorded in `docs/UPSTREAM-THEME-DEBT.md`. 32 ms is
/// saola-capture's own interim toast cadence, carried over unchanged — ~30
/// redraws a second, which reads as smooth for a rule that takes five
/// seconds to drain.
const REDRAW_INTERVAL: Duration = Duration::from_millis(32);

/// The toast stack's own message type, nested into `main.rs`'s outer
/// `Message` as `Message::Toast(..)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// One [`REDRAW_INTERVAL`] elapsed. Carries no timestamp: `main.rs`
    /// passes the `now` it read alongside the message, the same
    /// "Tick wakes the update, it doesn't carry the clock" shape
    /// saola-capture's toast and flash modules use.
    Tick,
    /// The pointer entered a card. Pauses **that card only** — §5's "hover
    /// pauses both" reads naturally as "the one you are pointing at", and
    /// per-card pause is what a `mouse_area` around a single card can
    /// express without the stack having to know which card is "the" one.
    Hovered(u32),
    /// The pointer left a card. Resumes that card's clock.
    Unhovered(u32),
    /// A card was clicked.
    Clicked(u32),
}

/// What a [`Message`] asks `main.rs` to do beyond the store mutation
/// [`Toasts::update`] has already made — the "return a value, not a `Task`"
/// shape saola-panel's `popover::Action` uses, for the same reason: it keeps
/// the decision unit-testable without a compositor or a bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// These notifications just left the screen, and why. `main.rs` emits
    /// `NotificationClosed(id, reason)` for each and resyncs the surface.
    /// Never empty — [`Toasts::update`] returns [`Action::None`] instead.
    Closed {
        ids: Vec<u32>,
        reason: crate::store::CloseReason,
    },
}

/// The toast surface's state.
///
/// Deliberately tiny: the cards, their clocks and the replace policy all
/// live in [`Store`] (the notification centre renders the same model — see
/// `modules/mod.rs`). What is genuinely view-level, and therefore lives
/// here, is which card the pointer is inside.
#[derive(Debug, Default)]
pub struct Toasts {
    hovered: Option<u32>,
}

impl Toasts {
    /// Which card the pointer is currently inside, if any.
    ///
    /// `main.rs` needs this on the `Notify` path: style guide §6 says a
    /// second notification from an app already on screen "replaces its card
    /// and resets the clock", and a reset stopwatch is a *running* stopwatch
    /// — so a card replaced under a stationary pointer would silently start
    /// counting down again, with no `Hovered` message coming (the pointer
    /// never moved, so it never re-entered anything). The daemon re-pauses
    /// that card itself; this is how it knows to.
    pub fn hovered(&self) -> Option<u32> {
        self.hovered
    }

    /// Folds one message into the store and reports what the daemon still
    /// has to do about it.
    pub fn update(
        &mut self,
        message: Message,
        store: &mut Store,
        limits: &crate::store::Limits,
        now: Instant,
    ) -> Action {
        match message {
            Message::Tick => {
                let ids = store.expire_toasts(now, limits);
                if ids.is_empty() {
                    Action::None
                } else {
                    Action::Closed {
                        ids,
                        reason: crate::store::CloseReason::Expired,
                    }
                }
            }
            Message::Hovered(id) => {
                self.hovered = Some(id);
                store.pause_toast(id, now);
                Action::None
            }
            Message::Unhovered(id) => {
                if self.hovered == Some(id) {
                    self.hovered = None;
                }
                store.resume_toast(id, now);
                Action::None
            }
            // Stage 6 ("Actions") splits this arm: a notification carrying a
            // `"default"` action fires it here instead, and only a card
            // without one dismisses on click. Until then every click is a
            // dismissal, which is what the freedesktop spec calls
            // `NotificationClosed` reason 2.
            Message::Clicked(id) => {
                if self.hovered == Some(id) {
                    self.hovered = None;
                }
                if store.dismiss_toast(id) {
                    Action::Closed {
                        ids: vec![id],
                        reason: crate::store::CloseReason::UserDismissed,
                    }
                } else {
                    Action::None
                }
            }
        }
    }

    /// The stack, newest card on top.
    ///
    /// An empty stack renders as nothing. `main.rs` unmaps the surface
    /// entirely at that point, so this arm is defensive: the frame between
    /// "the last card expired" and "the compositor destroyed the surface"
    /// still has to draw something.
    pub fn view<'a>(&self, theme: &Theme, store: &'a Store, now: Instant) -> Element<'a, Message> {
        let toasts = store.toasts();
        if toasts.is_empty() {
            return Space::new().into();
        }

        let mut stack = column![].spacing(theme.sizes.island_gap);
        // `.rev()` because `Store` keeps its stack oldest-first: the newest
        // card belongs directly under the panel, where the eye already is,
        // with older ones pushed down.
        for entry in toasts.iter().rev() {
            stack = stack.push(sliding_card(theme, entry, now));
        }
        stack.into()
    }

    /// Ticks only while a card is on screen — see [`REDRAW_INTERVAL`] for
    /// why this subscription exists at all and why the gate is the whole
    /// point of it.
    pub fn subscription(&self, store: &Store) -> Subscription<Message> {
        if store.toasts().is_empty() {
            Subscription::none()
        } else {
            iced::time::every(REDRAW_INTERVAL).map(|_instant| Message::Tick)
        }
    }
}

/// One card at its current point in the §5 envelope, behind the shrinking
/// leading spacer that stands in for the slide-in, wrapped in the
/// `mouse_area` that carries hover and click.
fn sliding_card<'a>(theme: &Theme, entry: &'a ToastEntry, now: Instant) -> Element<'a, Message> {
    let policy = rest_policy(theme, &entry.notification);
    let elapsed = entry.stopwatch.elapsed(now);
    let alpha = card_alpha(theme, policy, elapsed);
    let life = life_fraction(theme, policy, elapsed);
    let id = entry.notification.id;

    let slid = row![
        Space::new().width(Length::Fixed(slide_offset(theme, elapsed).max(0.0))),
        card_view(theme, &entry.notification, alpha, life),
    ];

    mouse_area(slid)
        .on_press(Message::Clicked(id))
        .on_enter(Message::Hovered(id))
        .on_exit(Message::Unhovered(id))
        .into()
}

/// §6's notification card, as a function of the notification alone.
///
/// **Kept free of toast state on purpose.** Stage 6 adds the action pills
/// inside this function, and Stage 7's notification centre renders the same
/// card for a history row — so everything that varies over a *toast's*
/// lifetime arrives as a parameter (`alpha` from [`card_alpha`], `life` from
/// [`life_fraction`]) rather than being read off a `ToastEntry` this
/// function cannot see. A caller with no animation to show passes
/// `alpha = 1.0` and `life = None`.
pub fn card_view<'a>(
    theme: &Theme,
    notification: &'a Notification,
    alpha: f32,
    life: Option<f32>,
) -> Element<'a, Message> {
    let title_size = theme.typography.size.body;
    let meta_size = theme.typography.size.meta;
    let body_size = theme.typography.size.secondary;
    let leading = iced::widget::text::LineHeight::Relative(theme.typography.line_height);

    // iced 0.14 has no subtree opacity, so the card's fade is applied to
    // every color it paints, one at a time — the same trick
    // `saola_theme::style::container::notification_card` performs on the
    // card's own chrome.
    let primary = theme.on_ink.primary.with_opacity(alpha);
    let secondary = theme.on_ink.secondary.with_opacity(alpha);
    let tertiary = theme.on_ink.tertiary.with_opacity(alpha);

    let header = row![
        text(&notification.summary)
            .font(saola_theme::convert::ui_font(theme))
            .size(title_size)
            .line_height(leading)
            .color(primary),
        Space::new().width(Length::Fill),
        text(&notification.app_name)
            .font(saola_theme::convert::ui_font_regular(theme))
            .size(meta_size)
            .line_height(leading)
            .color(tertiary),
    ]
    .align_y(Center);

    let body = text(&notification.body)
        .font(saola_theme::convert::ui_font_regular(theme))
        .size(body_size)
        .line_height(leading)
        .color(secondary);

    // The text block is given exactly the height `card_height` budgeted for
    // it and clipped, because a layer-shell surface's height is declared
    // before iced ever measures this text (see `card_height`): a body longer
    // than `BODY_LINES` must be cut off rather than push the card past the
    // surface it was sized for.
    let text_block_height = title_size * theme.typography.line_height
        + theme.sizes.gap_tight
        + body_size * theme.typography.line_height * BODY_LINES;
    let text_block = container(column![header, body].spacing(theme.sizes.gap_tight))
        .width(Length::Fill)
        .height(Length::Fixed(text_block_height))
        .clip(true);

    let content = row![icon_tile(theme, notification, alpha), text_block]
        .spacing(theme.sizes.pill_gap)
        .padding(theme.sizes.popover_padding)
        .align_y(Center);

    let card = container(column![content, life_rule(theme, life, alpha)])
        .width(Length::Fixed(theme.sizes.notification_card_width))
        .height(Length::Fixed(card_height(theme, notification)))
        .clip(true);

    // §6's urgent variant (concept 10b): "a terracotta ring and no life
    // rule". `saola_theme::style::container::card_urgent` is *not* the
    // helper for it here — that one is `card` plus the ring, and `card`'s
    // `Surface::Ink` arm paints an **ivory** card, where §6's notification
    // card is solid ink; it also takes no `alpha`, so it could not fade.
    // Composed from the two right pieces instead, both from the theme, and
    // recorded in `docs/UPSTREAM-THEME-DEBT.md`.
    if notification.urgency == Urgency::Critical {
        card.style(urgent_card_style(theme, alpha)).into()
    } else {
        card.style(saola_theme::style::container::notification_card(
            theme, alpha,
        ))
        .into()
    }
}

/// §6's "36px icon tile": the notification's own decoded image if it had
/// one, otherwise the themed tile with a generic glyph in it.
fn icon_tile<'a>(theme: &Theme, notification: &Notification, alpha: f32) -> Element<'a, Message> {
    let size = theme.sizes.icon_tile;

    let inner: Element<'a, Message> = match &notification.image {
        // `store::parse_hints` already downsampled this to `sizes.icon_tile`
        // and built a `Handle::from_rgba` (the one handle variant iced's
        // renderer resolves synchronously — see that function's own notes).
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(iced::ContentFit::Cover)
            .into(),
        None => saola_theme::icon(
            saola_theme::Icon::Info,
            theme.sizes.icon_menu,
            theme.on_ink.primary.with_opacity(alpha),
        )
        .into(),
    };

    container(inner)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .align_x(Center)
        .align_y(Center)
        .style(tile_style(theme, alpha))
        .clip(true)
        .into()
}

/// §6's "3px life rule across the bottom", or an empty strip of the same
/// height when this card has no rule (see [`life_fraction`]) — the empty
/// strip is what keeps an urgent card exactly as tall as a normal one.
fn life_rule<'a>(theme: &Theme, life: Option<f32>, alpha: f32) -> Element<'a, Message> {
    let girth = theme.sizes.life_rule;
    match life {
        Some(remaining) => progress_bar(0.0..=1.0, remaining)
            .length(Length::Fill)
            .girth(Length::Fixed(girth))
            .style(life_rule_style(theme, alpha))
            .into(),
        None => Space::new().height(Length::Fixed(girth)).into(),
    }
}

/// [`saola_theme::style::notification::icon_tile`]'s recipe with the card's
/// fade applied. The theme's own helper takes no `alpha` — see this file's
/// urgent-card comment and `docs/UPSTREAM-THEME-DEBT.md`; every value here
/// is still a token, nothing is invented.
fn tile_style(
    theme: &Theme,
    alpha: f32,
) -> impl Fn(&iced::Theme) -> container::Style + Clone + use<> {
    let background = theme.on_ink.fill_subtle.with_opacity(alpha);
    let text_color = theme.on_ink.primary.with_opacity(alpha);
    let border = saola_theme::style::border_none(theme.radii.tile);
    move |_| container::Style {
        text_color: Some(text_color),
        background: Some(iced::Background::Color(background)),
        border,
        ..container::Style::default()
    }
}

/// [`saola_theme::style::notification::life_rule`]'s recipe with the card's
/// fade applied — same gap, same posture, as [`tile_style`].
fn life_rule_style(
    theme: &Theme,
    alpha: f32,
) -> impl Fn(&iced::Theme) -> progress_bar::Style + Clone + use<> {
    let track = theme.on_ink.fill_subtle.with_opacity(alpha);
    let accent = theme.palette.accent.with_opacity(alpha);
    move |_| progress_bar::Style {
        background: iced::Background::Color(track),
        bar: iced::Background::Color(accent),
        border: saola_theme::style::border_none(0.0),
    }
}

/// [`saola_theme::style::container::notification_card`] plus
/// [`saola_theme::style::accent_ring`] — §6's ink card wearing concept 10b's
/// terracotta ring, at the card's current alpha.
fn urgent_card_style(
    theme: &Theme,
    alpha: f32,
) -> impl Fn(&iced::Theme) -> container::Style + Clone + use<> {
    let base = saola_theme::style::container::notification_card(theme, alpha);
    let mut ring = saola_theme::style::accent_ring(theme, theme.radii.card);
    ring.color.a *= alpha.clamp(0.0, 1.0);
    move |t| container::Style {
        border: ring,
        ..base(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::saola()
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// A notification with everything a card needs and nothing surprising.
    fn notification(id: u32, urgency: Urgency, expire_timeout: i32) -> Notification {
        Notification {
            id,
            app_name: "test-app".to_string(),
            app_icon: String::new(),
            summary: "Summary".to_string(),
            body: "Body".to_string(),
            actions: Vec::new(),
            urgency,
            image: None,
            expire_timeout,
            transient: false,
            resident: false,
            posted_at: Instant::now(),
        }
    }

    fn entry(id: u32) -> ToastEntry {
        ToastEntry {
            notification: notification(id, Urgency::Normal, -1),
            stopwatch: crate::store::Stopwatch::started(Instant::now()),
        }
    }

    // -- the envelope -------------------------------------------------------

    #[test]
    fn envelope_is_the_entrance_plus_the_exit() {
        let t = theme();
        assert_eq!(
            envelope(&t),
            ms(u64::from(t.motion.toast_in) + u64::from(t.motion.toast_out))
        );
    }

    #[test]
    fn a_default_toasts_whole_life_is_the_style_guides_total() {
        let t = theme();
        let policy = rest_policy(&t, &notification(1, Urgency::Normal, -1));
        let ExpiryPolicy::After(rest) = policy else {
            panic!("a normal notification with expire_timeout -1 expires");
        };
        assert_eq!(
            rest + envelope(&t),
            ms(u64::from(t.motion.toast_total)),
            "§5: 0.35 s in + 5 s rest + 1 s out == the theme's toast_total"
        );
    }

    #[test]
    fn rest_policy_follows_urgency_and_expire_timeout() {
        let t = theme();
        assert_eq!(
            rest_policy(&t, &notification(1, Urgency::Normal, -1)),
            ExpiryPolicy::After(ms(u64::from(t.motion.toast_idle)))
        );
        assert_eq!(
            rest_policy(&t, &notification(1, Urgency::Normal, 2000)),
            ExpiryPolicy::After(ms(2000))
        );
        assert_eq!(
            rest_policy(&t, &notification(1, Urgency::Normal, 0)),
            ExpiryPolicy::Never
        );
        assert_eq!(
            rest_policy(&t, &notification(1, Urgency::Critical, -1)),
            ExpiryPolicy::Never
        );
    }

    // -- card_alpha ---------------------------------------------------------

    /// The local generalization must agree with saola-theme's own §5
    /// encoding wherever the theme can express the same thing. If this ever
    /// fails, the toast has drifted off the style guide.
    #[test]
    fn card_alpha_matches_the_theme_for_a_default_rest_span() {
        let t = theme();
        let policy = ExpiryPolicy::After(ms(u64::from(t.motion.toast_idle)));
        for millis in [
            0, 1, 175, 349, 350, 351, 2500, 5349, 5350, 5900, 6349, 6350, 9000,
        ] {
            let elapsed = ms(millis);
            assert_eq!(
                card_alpha(&t, policy, elapsed),
                saola_theme::motion::toast_alpha(&t, elapsed),
                "diverged from the theme's §5 envelope at {millis} ms"
            );
        }
    }

    #[test]
    fn card_alpha_fades_in_then_holds_forever_for_a_card_that_never_leaves() {
        let t = theme();
        let in_ms = u64::from(t.motion.toast_in);

        assert_eq!(card_alpha(&t, ExpiryPolicy::Never, ms(0)), 0.0);
        assert_eq!(card_alpha(&t, ExpiryPolicy::Never, ms(in_ms)), 1.0);
        assert_eq!(
            card_alpha(&t, ExpiryPolicy::Never, Duration::from_secs(3600)),
            1.0,
            "an urgent card never fades out"
        );
    }

    #[test]
    fn card_alpha_fades_out_over_the_exit_after_a_custom_rest_span() {
        let t = theme();
        let in_ms = u64::from(t.motion.toast_in);
        let out_ms = u64::from(t.motion.toast_out);
        let policy = ExpiryPolicy::After(ms(2000));

        assert_eq!(
            card_alpha(&t, policy, ms(in_ms + 1000)),
            1.0,
            "still at rest"
        );
        assert_eq!(
            card_alpha(&t, policy, ms(in_ms + 2000)),
            1.0,
            "the fade-out has not started yet"
        );
        let mid = card_alpha(&t, policy, ms(in_ms + 2000 + out_ms / 2));
        assert!(mid > 0.0 && mid < 1.0, "half-faded, got {mid}");
        assert_eq!(card_alpha(&t, policy, ms(in_ms + 2000 + out_ms)), 0.0);
    }

    // -- life_fraction ------------------------------------------------------

    #[test]
    fn life_fraction_matches_the_theme_for_a_default_rest_span() {
        let t = theme();
        let policy = ExpiryPolicy::After(ms(u64::from(t.motion.toast_idle)));
        for millis in [0, 1, 349, 350, 351, 2850, 5349, 5350, 5900, 6350, 9000] {
            let elapsed = ms(millis);
            assert_eq!(
                life_fraction(&t, policy, elapsed),
                Some(saola_theme::motion::life_fraction(&t, elapsed)),
                "diverged from the theme's §5 life rule at {millis} ms"
            );
        }
    }

    #[test]
    fn a_card_that_never_leaves_has_no_life_rule() {
        let t = theme();
        assert_eq!(life_fraction(&t, ExpiryPolicy::Never, ms(0)), None);
        assert_eq!(
            life_fraction(&t, ExpiryPolicy::Never, Duration::from_secs(3600)),
            None
        );
    }

    #[test]
    fn life_fraction_drains_across_a_custom_rest_span() {
        let t = theme();
        let in_ms = u64::from(t.motion.toast_in);
        let policy = ExpiryPolicy::After(ms(2000));

        assert_eq!(life_fraction(&t, policy, ms(0)), Some(1.0));
        assert_eq!(
            life_fraction(&t, policy, ms(in_ms)),
            Some(1.0),
            "full through the entrance — nothing to count down yet"
        );
        let half = life_fraction(&t, policy, ms(in_ms + 1000)).expect("a normal card has a rule");
        assert!((half - 0.5).abs() < 0.01, "got {half}");
        assert_eq!(life_fraction(&t, policy, ms(in_ms + 2000)), Some(0.0));
        assert_eq!(life_fraction(&t, policy, ms(in_ms + 9000)), Some(0.0));
    }

    // -- slide_offset -------------------------------------------------------

    #[test]
    fn slide_offset_travels_a_full_card_width_over_the_entrance() {
        let t = theme();
        assert_eq!(slide_offset(&t, ms(0)), t.sizes.notification_card_width);
        assert_eq!(slide_offset(&t, ms(u64::from(t.motion.toast_in))), 0.0);
        assert_eq!(
            slide_offset(&t, Duration::from_secs(60)),
            0.0,
            "the card stays put once it has arrived"
        );

        let quarter = slide_offset(&t, ms(u64::from(t.motion.toast_in) / 4));
        let half = slide_offset(&t, ms(u64::from(t.motion.toast_in) / 2));
        assert!(
            quarter > half && half > 0.0,
            "travel is monotonic: {quarter} then {half}"
        );
    }

    // -- surface geometry ---------------------------------------------------

    #[test]
    fn a_card_is_tall_enough_for_its_tile_its_text_and_its_rule() {
        let t = theme();
        let height = card_height(&t, &notification(1, Urgency::Normal, -1));
        let floor = t.sizes.popover_padding * 2.0 + t.sizes.icon_tile + t.sizes.life_rule;
        assert!(
            height >= floor,
            "a card must clear its own padding, tile and rule: {height} < {floor}"
        );
    }

    #[test]
    fn an_urgent_card_is_the_same_height_as_a_normal_one() {
        let t = theme();
        assert_eq!(
            card_height(&t, &notification(1, Urgency::Critical, -1)),
            card_height(&t, &notification(2, Urgency::Normal, -1)),
            "a missing life rule must not change the stack's rhythm"
        );
    }

    #[test]
    fn an_empty_stack_has_no_height() {
        assert_eq!(stack_height(&theme(), &[]), 0);
    }

    #[test]
    fn each_extra_card_adds_its_height_and_one_gap() {
        let t = theme();
        let one = stack_height(&t, &[entry(1)]);
        let two = stack_height(&t, &[entry(1), entry(2)]);
        let three = stack_height(&t, &[entry(1), entry(2), entry(3)]);

        let card = card_height(&t, &notification(1, Urgency::Normal, -1));
        let step = (card + t.sizes.island_gap).round() as u32;
        assert_eq!(two - one, step);
        assert_eq!(three - two, step);
    }
}
