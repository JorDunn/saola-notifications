//! The notification centre: style guide §6's "Notification centre" surface —
//! 460 px wide, anchored 72 px from the top and 26 px from the right, its
//! history grouped by application into collapsible groups, with a
//! do-not-disturb toggle and a clear-all row.
//!
//! # What this module owns, and what it does not (teaching note)
//!
//! Almost nothing. The notifications, the collapsed-group set and every
//! mutation of both live in [`crate::store::Store`] — the toast stack renders
//! the same model, so neither surface may own it (see `modules/mod.rs`). The
//! one genuinely view-level fact the centre owns is **whether it is open**,
//! and that is the whole of [`Centre`].
//!
//! Surface geometry is `main.rs`'s: this module supplies the pure arithmetic
//! ([`centre_height`], [`surface_height`]) and `main.rs` turns the answer into
//! a layer-shell surface. The split is the same one the toast module already
//! makes with `stack_height`, and it exists for the same reason — a
//! layer-shell surface's height has to be declared *before* iced measures
//! anything, so the height must be computable from the model alone.
//!
//! # Where the clamp comes from (teaching note)
//!
//! §6 caps the centre at `calc(100% - 98px)`, and 98 is not a number this
//! file invents: it is `sizes.popover_top` (72, the offset from the screen
//! top) plus `sizes.panel_margin_islands` (26, §6's "26px from the relevant
//! edge"), the same two tokens the surface's own margins are built from. What
//! this module cannot know is the `100%` — the output's height — because
//! `iced_layershell` 0.19 exposes no output geometry to an application.
//! `main.rs` therefore *measures* it: the first time the centre opens it
//! spawns the surface anchored top **and** bottom with a height of zero,
//! which the layer-shell protocol defines as "compositor, stretch this", and
//! the configure event that comes back is exactly `output_height − 98`. That
//! number is fed to [`surface_height`] as `max_height` from then on. See
//! `main.rs`'s `CentreClamp` for the whole dance, including what happens when
//! a compositor refuses to answer.
//!
//! # Reuse, not re-implementation
//!
//! A history row is [`crate::modules::toast::card_view`] with `alpha = 1.0`
//! and `life = None` — the same card, minus the two things that only mean
//! something to a *toast* (its fade and its countdown). That is what Stage 6
//! kept `card_view` free of toast state for. `card_height` follows, so
//! [`centre_height`] never has to know what a card is made of.

use iced::widget::{Space, column, container, mouse_area, row, scrollable, toggler};
use iced::{Element, Length, Subscription, window};
use saola_theme::{Surface, Theme};

use crate::modules::toast;
use crate::store::{Notification, Store, invoke_action_policy};

// ---------------------------------------------------------------------
// The view model: history, grouped by application.
// ---------------------------------------------------------------------

/// One application's slice of history, newest notification first.
///
/// Borrowed from the store rather than cloned: this is rebuilt on every
/// frame and every height calculation, and a notification carries a decoded
/// image handle that has no business being copied thirty times a second.
#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    pub app_name: &'a str,
    /// Whether this group's rows are hidden — [`Store::is_collapsed`],
    /// resolved here so [`centre_height`] and [`view`](Centre::view) read the
    /// same answer without either of them touching the store again.
    pub collapsed: bool,
    /// Newest first.
    pub notifications: Vec<&'a Notification>,
}

/// Groups the store's flat history by `app_name`, **at view time** — the
/// store deliberately stays flat (see [`Store::history`]).
///
/// Ordering, both levels, is newest first: history is kept oldest-first, so
/// this walks it in reverse. A group's position is therefore decided by its
/// most recent notification, which is what makes a freshly-arrived
/// notification pull its application to the top of the centre.
pub fn group_history(store: &Store) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();

    for notification in store.history().iter().rev() {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.app_name == notification.app_name)
        {
            group.notifications.push(notification);
        } else {
            groups.push(Group {
                app_name: &notification.app_name,
                collapsed: store.is_collapsed(&notification.app_name),
                notifications: vec![notification],
            });
        }
    }

    groups
}

// ---------------------------------------------------------------------
// Surface geometry — pure, and called before any surface exists.
// ---------------------------------------------------------------------

/// One group's block height: its header row, plus (when the group is
/// expanded) every card and the `sizes.gap_tight` that separates each card
/// from what is above it.
///
/// A collapsed group is exactly its header — that is the whole point of
/// collapsing it, and it is what lets a user shrink an overflowing centre
/// back under the clamp.
pub fn group_height(theme: &Theme, group: &Group<'_>) -> f32 {
    let header = theme.sizes.list_row;
    if group.collapsed {
        return header;
    }
    let cards: f32 = group
        .notifications
        .iter()
        .map(|notification| theme.sizes.gap_tight + toast::card_height(theme, notification))
        .sum();
    header + cards
}

/// The centre's **hug** height: exactly as tall as its content wants to be,
/// before any clamp. §6: "It hugs its content and only reaches full height
/// when there is enough to show."
///
/// The three fixed pieces (`chrome` below) are the popover's own vertical
/// padding, the title/do-not-disturb row, and the gap under it. An empty
/// centre adds one list row for the empty-state line and no clear-all row —
/// there is nothing to clear. A non-empty one adds the group blocks, the
/// gaps between them, and the clear-all row with its own gap above it.
///
/// Every term is a token. Nothing here is measured text: a layer-shell
/// surface is sized before iced lays anything out, which is also why
/// [`toast::card_height`] budgets a fixed two-line body (see its own note).
pub fn centre_height(theme: &Theme, groups: &[Group<'_>]) -> f32 {
    let chrome = theme.sizes.popover_padding * 2.0 + theme.sizes.list_row + theme.sizes.island_gap;

    if groups.is_empty() {
        return chrome + theme.sizes.list_row;
    }

    let blocks: f32 = groups
        .iter()
        .map(|group| group_height(theme, group))
        .sum::<f32>()
        + (groups.len() - 1) as f32 * theme.sizes.island_gap;

    chrome + blocks + theme.sizes.island_gap + theme.sizes.hit_target_bar
}

/// The centre surface's declared height in logical pixels: [`centre_height`]
/// clamped to `max_height` when `main.rs` has measured one.
///
/// `max_height` is `None` only while the clamp is unknown or the compositor
/// declined to supply it (see this module's doc comment); an unclamped
/// surface can overhang the screen bottom, which costs the rows below the
/// fold but never crashes anything.
pub fn surface_height(theme: &Theme, groups: &[Group<'_>], max_height: Option<u32>) -> u32 {
    // `max(1.0)` rather than `max(0.0)`: a zero-height layer-shell surface
    // anchored to one edge means "compositor, you decide", which is the exact
    // opposite of what a hug-height surface is asking for.
    let hug = centre_height(theme, groups).round().max(1.0) as u32;
    match max_height {
        Some(max) => hug.min(max.max(1)),
        None => hug,
    }
}

// ---------------------------------------------------------------------
// The module: state, messages, update, view, subscription.
// ---------------------------------------------------------------------

/// The centre's own message type, nested into `main.rs`'s outer `Message` as
/// `Message::Centre(..)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A group header was clicked — collapse or expand that application.
    ToggleGroup(String),
    /// A history row was clicked outside any action pill: dismiss it.
    Dismiss(u32),
    /// The clear-all row was clicked.
    ClearAll,
    /// The do-not-disturb toggle moved. Manual DND only — the recording
    /// half of `effective_dnd` is saola-capture's to set (Stage 8).
    SetDnd(bool),
    /// Something inside a reused [`toast::card_view`] fired. In practice
    /// this is only ever [`toast::Message::ActionClicked`] — `card_view`
    /// builds no other message; the hover/tick/click messages belong to the
    /// `mouse_area` the *toast* module wraps its cards in, and the centre
    /// wraps its own.
    Card(toast::Message),
    /// Escape was pressed. Not filtered by surface: the centre is the only
    /// keyboard-interactive surface this daemon has
    /// (`KeyboardInteractivity::OnDemand`; the toast stack is `None`), so a
    /// key event reaching this process at all was aimed at the centre —
    /// and `iced_layershell` attributes a key event with no surface of its
    /// own to whichever window it happens to iterate first.
    EscapePressed,
    /// A surface lost keyboard focus. Carries the surface's own id, because
    /// `iced::event::listen_with` is process-wide and takes a plain `fn`
    /// pointer — it cannot capture the centre's id to filter on.
    FocusLost(window::Id),
}

/// What a [`Message`] asks `main.rs` to do beyond the store mutation
/// [`Centre::update`] has already made — the same "return a value, not a
/// `Task`" shape [`toast::Action`] uses, and for the same reason: it keeps
/// the decision unit-testable without a compositor or a bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// The centre closed itself (Escape, focus loss). [`Centre::is_open`] is
    /// already `false`; `main.rs` only has to unmap the surface.
    Close,
    /// These notifications were dismissed from the centre. `main.rs` emits
    /// `NotificationClosed(id, 2)` for each — §6 dismissals are always
    /// user-dismissals. Never empty.
    Closed(Vec<u32>),
    /// An action pill inside a history card was clicked. `main.rs` always
    /// emits `ActionInvoked(id, key)`; `closed` is
    /// [`crate::store::invoke_action_policy`]'s answer, already applied to
    /// the store, and tells `main.rs` whether to *also* emit
    /// `NotificationClosed(id, 2)`.
    Invoked {
        id: u32,
        key: String,
        closed: bool,
    },
    /// The do-not-disturb toggle moved; `main.rs` owns the manual DND flag.
    Dnd(bool),
}

/// The notification centre's state: whether it is open, and nothing else.
///
/// `main.rs` mirrors this into a surface (`centre_surface`) rather than the
/// other way round, so that "is the centre open?" — which Stage 9's
/// `io.saola.Notifications1.CentreOpen` property has to answer, and which a
/// `ToggleCentre` call has to flip — is a plain `bool` in the model rather
/// than a question about the compositor's current window list.
#[derive(Debug, Default)]
pub struct Centre {
    open: bool,
}

impl Centre {
    /// Whether the centre is open. **Stage 9 reads this for the
    /// `CentreOpen` property**; it changes only inside this file and
    /// `main.rs`'s three `*Centre` control-method arms, so every change site
    /// is a place to emit `PropertiesChanged` from.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// `io.saola.Notifications1.OpenCentre` / `CloseCentre`. Returns whether
    /// the flag actually moved, so a caller can skip re-emitting a property
    /// that did not change.
    pub fn set_open(&mut self, open: bool) -> bool {
        let changed = self.open != open;
        self.open = open;
        changed
    }

    /// `io.saola.Notifications1.ToggleCentre` — "toggling while open closes"
    /// (PLAN.md Stage 7) falls straight out of this. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        self.open
    }

    /// Folds one message into the store and reports what the daemon still
    /// has to do about it.
    ///
    /// `surface` is the centre surface's id while one is mapped — needed
    /// only to tell *this* surface's focus loss from any other surface's;
    /// see [`Message::FocusLost`].
    pub fn update(
        &mut self,
        message: Message,
        store: &mut Store,
        surface: Option<window::Id>,
    ) -> Action {
        match message {
            Message::ToggleGroup(app_name) => {
                store.toggle_collapsed(&app_name);
                Action::None
            }

            Message::Dismiss(id) => {
                if store.dismiss_notification(id) {
                    Action::Closed(vec![id])
                } else {
                    Action::None
                }
            }

            Message::ClearAll => {
                let ids = store.clear_all();
                if ids.is_empty() {
                    Action::None
                } else {
                    Action::Closed(ids)
                }
            }

            Message::SetDnd(manual) => Action::Dnd(manual),

            Message::Card(toast::Message::ActionClicked(id, key)) => {
                // The centre can show a notification whose toast has long
                // since expired, so `resident` is looked up in history first
                // and the stack second. An id in neither is a pill from a row
                // that has just been dismissed under the pointer: report
                // nothing rather than emit `ActionInvoked` for a
                // notification this daemon no longer has.
                let Some(resident) = store
                    .history()
                    .iter()
                    .chain(store.toasts().iter().map(|toast| &toast.notification))
                    .find(|notification| notification.id == id)
                    .map(|notification| notification.resident)
                else {
                    return Action::None;
                };

                let policy = invoke_action_policy(resident);
                if policy.close_after {
                    store.dismiss_notification(id);
                }
                Action::Invoked {
                    id,
                    key,
                    closed: policy.close_after,
                }
            }

            // `card_view` builds no other message (see [`Message::Card`]);
            // this arm is the no-panic posture, not a live path.
            Message::Card(_) => Action::None,

            Message::EscapePressed => {
                if self.open {
                    self.open = false;
                    Action::Close
                } else {
                    Action::None
                }
            }

            Message::FocusLost(id) => {
                if self.open && surface == Some(id) {
                    self.open = false;
                    Action::Close
                } else {
                    Action::None
                }
            }
        }
    }

    /// §6's centre: a title-and-DND row, the grouped history in a
    /// `scrollable`, and a clear-all row.
    ///
    /// # Why the horizontal insets are not all the same (teaching note)
    ///
    /// The surface is `sizes.notification_centre_width` (460) wide and a card
    /// is `sizes.notification_card_width` (440) — a difference of exactly two
    /// `sizes.island_gap`s. So the popover's own horizontal padding is
    /// `island_gap`, which makes the card list land at its natural width with
    /// nothing to clip, and the text rows above and below take a second
    /// `island_gap` of their own to sit at `sizes.popover_padding` (20) from
    /// the surface edge, where §6 puts a popover's text.
    pub fn view<'a>(
        &self,
        theme: &Theme,
        store: &'a Store,
        dnd_manual: bool,
    ) -> Element<'a, Message> {
        let groups = group_history(store);

        let mut stack = column![header_row(theme, dnd_manual)]
            .spacing(theme.sizes.island_gap)
            .height(Length::Fill);

        if groups.is_empty() {
            stack = stack.push(
                container(saola_theme::widget::empty_state(
                    theme,
                    Surface::Ink,
                    "No notifications",
                ))
                .height(Length::Fixed(theme.sizes.list_row)),
            );
        } else {
            let mut list = column![].spacing(theme.sizes.island_gap);
            for group in &groups {
                list = list.push(group_block(theme, group));
            }
            // The scrollable is what turns the clamp into scrolling rather
            // than clipping: it is `Fill`, so it takes whatever height is
            // left after the two fixed rows — the whole hug height when the
            // centre fits, and the clamped remainder when it does not.
            stack = stack.push(
                scrollable(list)
                    .height(Length::Fill)
                    .style(saola_theme::style::scrollable::rest(theme, Surface::Ink)),
            );
            stack = stack.push(clear_all_row(theme));
        }

        container(stack)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([theme.sizes.popover_padding, theme.sizes.island_gap])
            .style(saola_theme::style::container::popover(theme))
            .into()
    }

    /// Escape and focus loss, and only while the centre is open — the centre
    /// is not an animated surface, so this is the module's only subscription
    /// and it maps to real events rather than a tick (AGENTS.md's "every
    /// module maps to a signal, never a poll").
    ///
    /// `iced::event::listen_with` takes a **plain `fn` pointer**, not a
    /// closure, so nothing can be captured here — which is why
    /// [`Message::FocusLost`] carries the surface id for
    /// [`Centre::update`] to filter on instead.
    pub fn subscription(&self) -> Subscription<Message> {
        if !self.open {
            return Subscription::none();
        }

        iced::event::listen_with(|event, _status, id| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) => Some(Message::EscapePressed),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::FocusLost(id)),
            _ => None,
        })
    }
}

/// The title row: §6's section label on the left, the do-not-disturb toggle
/// on the right, in one `sizes.list_row`-tall band.
fn header_row<'a>(theme: &Theme, dnd_manual: bool) -> Element<'a, Message> {
    let toggle = row![
        saola_theme::widget::text::body(theme, Surface::Ink, "Do not disturb"),
        toggler(dnd_manual)
            .on_toggle(Message::SetDnd)
            .style(saola_theme::style::toggles::toggler(theme, Surface::Ink)),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(iced::Center);

    let content = row![
        saola_theme::widget::text::label(theme, Surface::Ink, "NOTIFICATIONS"),
        Space::new().width(Length::Fill),
        toggle,
    ]
    .align_y(iced::Center);

    container(saola_theme::widget::list_row_container(theme, content))
        .padding([0.0, theme.sizes.island_gap])
        .into()
}

/// One application's block: the theme's own group-header row (label, count
/// chip, chevron) and, unless the group is collapsed, its cards.
fn group_block<'a>(theme: &Theme, group: &Group<'a>) -> Element<'a, Message> {
    let mut block = column![saola_theme::widget::group_header(
        theme,
        group.app_name,
        group.notifications.len(),
        group.collapsed,
        Some(Message::ToggleGroup(group.app_name.to_string())),
    )]
    .spacing(theme.sizes.gap_tight);

    if !group.collapsed {
        for notification in &group.notifications {
            block = block.push(history_card(theme, notification));
        }
    }

    block.into()
}

/// One history row: Stage 6's reusable card, still, with a click that
/// dismisses it.
///
/// **A click dismisses rather than firing the notification's `"default"`
/// action**, which is the one place the centre deliberately differs from a
/// toast. A toast is the notification arriving and clicking it is answering
/// it; the centre is the list of what has already arrived, and clicking a
/// row there means "I am done with this". The pills are still live for
/// anyone who wants the action — and a `"default"` action renders no pill
/// (`store::action_pills`), so the one thing this costs is a pill-less
/// default action, which is unreachable from the centre by design rather
/// than by accident.
///
/// The `mouse_area` sits *outside* the card, so a pill's own `button`
/// captures its click first and this `on_press` never fires for it (iced
/// 0.14's `mouse_area` updates its child before its own press handling —
/// verified against the widget's source in Stage 6).
fn history_card<'a>(theme: &Theme, notification: &'a Notification) -> Element<'a, Message> {
    let id = notification.id;
    mouse_area(toast::card_view(theme, notification, 1.0, None).map(Message::Card))
        .on_press(Message::Dismiss(id))
        .into()
}

/// The clear-all row: one right-aligned pill, `sizes.hit_target_bar` tall.
fn clear_all_row<'a>(theme: &Theme) -> Element<'a, Message> {
    let button = saola_theme::widget::icon_button(
        theme,
        Surface::Ink,
        saola_theme::Icon::Trash2,
        Some("Clear all"),
        saola_theme::widget::role(theme, Surface::Ink, saola_theme::widget::Emphasis::Quiet),
        Some(Message::ClearAll),
    );

    container(row![Space::new().width(Length::Fill), button])
        .padding([0.0, theme.sizes.island_gap])
        .into()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::store::{Action as StoreAction, Limits, Urgency};

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    fn theme() -> Theme {
        Theme::saola()
    }

    fn limits() -> Limits {
        Limits {
            icon_tile: 36.0,
            toast_idle_ms: 5000,
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

    /// A store whose history is `apps` in order, all suppressed so nothing
    /// lands on the toast stack unless a test asks for it.
    fn store_with(apps: &[(u32, &str)]) -> Store {
        let now = Instant::now();
        let mut store = Store::new();
        for (id, app) in apps {
            store.notify(notification(*id, app, now), 0, true, now, &limits());
        }
        store
    }

    // ------------------------------------------------------------------
    // Grouping
    // ------------------------------------------------------------------

    #[test]
    fn an_empty_history_has_no_groups() {
        assert!(group_history(&Store::new()).is_empty());
    }

    #[test]
    fn notifications_from_one_app_share_a_group() {
        let store = store_with(&[(1, "slack"), (2, "slack"), (3, "slack")]);
        let groups = group_history(&store);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].app_name, "slack");
        assert_eq!(groups[0].notifications.len(), 3);
    }

    #[test]
    fn groups_and_rows_are_both_newest_first() {
        let store = store_with(&[(1, "slack"), (2, "mail"), (3, "slack")]);
        let groups = group_history(&store);

        assert_eq!(
            groups.iter().map(|g| g.app_name).collect::<Vec<_>>(),
            vec!["slack", "mail"],
            "slack's newest notification (id 3) is newer than mail's, so slack leads"
        );
        assert_eq!(
            groups[0]
                .notifications
                .iter()
                .map(|n| n.id)
                .collect::<Vec<_>>(),
            vec![3, 1],
            "within a group the newest row is on top"
        );
    }

    #[test]
    fn a_group_reports_the_stores_collapsed_state() {
        let mut store = store_with(&[(1, "slack"), (2, "mail")]);
        store.toggle_collapsed("mail");

        let groups = group_history(&store);
        let mail = groups.iter().find(|g| g.app_name == "mail").unwrap();
        let slack = groups.iter().find(|g| g.app_name == "slack").unwrap();

        assert!(mail.collapsed);
        assert!(!slack.collapsed);
    }

    // ------------------------------------------------------------------
    // centre_height
    // ------------------------------------------------------------------

    #[test]
    fn an_empty_centre_is_its_chrome_plus_one_empty_state_row() {
        let theme = theme();
        let expected = theme.sizes.popover_padding * 2.0
            + theme.sizes.list_row
            + theme.sizes.island_gap
            + theme.sizes.list_row;

        assert_eq!(centre_height(&theme, &[]), expected);
    }

    #[test]
    fn one_group_of_one_adds_its_header_its_card_and_the_clear_all_row() {
        let theme = theme();
        let store = store_with(&[(1, "slack")]);
        let groups = group_history(&store);

        let empty_chrome =
            theme.sizes.popover_padding * 2.0 + theme.sizes.list_row + theme.sizes.island_gap;
        let expected = empty_chrome
            + theme.sizes.list_row
            + theme.sizes.gap_tight
            + toast::card_height(&theme, groups[0].notifications[0])
            + theme.sizes.island_gap
            + theme.sizes.hit_target_bar;

        assert_eq!(centre_height(&theme, &groups), expected);
    }

    #[test]
    fn a_second_group_adds_its_own_block_and_one_gap() {
        let theme = theme();
        let one = store_with(&[(1, "slack")]);
        let two = store_with(&[(1, "slack"), (2, "mail")]);

        let one_group = group_history(&one);
        let two_groups = group_history(&two);

        let block = group_height(&theme, &two_groups[0]);
        assert_eq!(
            centre_height(&theme, &two_groups) - centre_height(&theme, &one_group),
            block + theme.sizes.island_gap
        );
    }

    #[test]
    fn collapsing_a_group_shrinks_the_centre_to_that_groups_header() {
        let theme = theme();
        let mut store = store_with(&[(1, "slack"), (2, "slack")]);
        let expanded = centre_height(&theme, &group_history(&store));

        store.toggle_collapsed("slack");
        let collapsed = centre_height(&theme, &group_history(&store));

        let cards: f32 = group_history(&store)[0]
            .notifications
            .iter()
            .map(|n| theme.sizes.gap_tight + toast::card_height(&theme, n))
            .sum();
        assert_eq!(expanded - collapsed, cards);
        assert!(collapsed < expanded);
    }

    #[test]
    fn every_group_collapsed_is_the_shortest_a_non_empty_centre_gets() {
        let theme = theme();
        let mut store = store_with(&[(1, "slack"), (2, "mail")]);
        store.toggle_collapsed("slack");
        store.toggle_collapsed("mail");

        let groups = group_history(&store);
        let expected = theme.sizes.popover_padding * 2.0
            + theme.sizes.list_row
            + theme.sizes.island_gap
            + theme.sizes.list_row * 2.0
            + theme.sizes.island_gap
            + theme.sizes.island_gap
            + theme.sizes.hit_target_bar;

        assert_eq!(centre_height(&theme, &groups), expected);
    }

    // ------------------------------------------------------------------
    // surface_height — the clamp
    // ------------------------------------------------------------------

    #[test]
    fn an_unclamped_surface_is_the_hug_height() {
        let theme = theme();
        let store = store_with(&[(1, "slack")]);
        let groups = group_history(&store);

        assert_eq!(
            surface_height(&theme, &groups, None),
            centre_height(&theme, &groups).round() as u32
        );
    }

    #[test]
    fn a_clamp_taller_than_the_content_leaves_the_hug_height_alone() {
        let theme = theme();
        let store = store_with(&[(1, "slack")]);
        let groups = group_history(&store);
        let hug = centre_height(&theme, &groups).round() as u32;

        assert_eq!(surface_height(&theme, &groups, Some(hug + 500)), hug);
    }

    #[test]
    fn overflowing_content_is_cut_to_the_clamp() {
        let theme = theme();
        let apps: Vec<(u32, String)> = (1..=40).map(|i| (i, format!("app-{i}"))).collect();
        let borrowed: Vec<(u32, &str)> = apps.iter().map(|(id, app)| (*id, app.as_str())).collect();
        let store = store_with(&borrowed);
        let groups = group_history(&store);

        // 1080 − 98: a 1080p output's answer, the shape the compositor
        // reports back from the measure surface.
        let clamp = 1080 - 98;
        assert!(
            centre_height(&theme, &groups) > clamp as f32,
            "forty groups must overflow a 1080p screen, or this test proves nothing"
        );
        assert_eq!(surface_height(&theme, &groups, Some(clamp)), clamp);
    }

    #[test]
    fn a_degenerate_clamp_never_asks_for_a_zero_height_surface() {
        let theme = theme();
        assert_eq!(
            surface_height(&theme, &[], Some(0)),
            1,
            "a zero-height layer-shell surface means \"compositor, you decide\" — the exact \
             opposite of a hug-height request"
        );
    }

    // ------------------------------------------------------------------
    // Open / close state
    // ------------------------------------------------------------------

    #[test]
    fn a_new_centre_is_closed() {
        assert!(!Centre::default().is_open());
    }

    #[test]
    fn toggling_while_open_closes() {
        let mut centre = Centre::default();
        assert!(centre.toggle());
        assert!(!centre.toggle());
    }

    #[test]
    fn set_open_reports_only_a_real_change() {
        let mut centre = Centre::default();
        assert!(centre.set_open(true));
        assert!(!centre.set_open(true), "already open — nothing moved");
        assert!(centre.set_open(false));
    }

    // ------------------------------------------------------------------
    // update
    // ------------------------------------------------------------------

    #[test]
    fn toggling_a_group_flips_it_in_the_store() {
        let mut centre = Centre::default();
        let mut store = store_with(&[(1, "slack")]);

        let action = centre.update(Message::ToggleGroup("slack".to_string()), &mut store, None);

        assert_eq!(action, Action::None);
        assert!(store.is_collapsed("slack"));
    }

    #[test]
    fn dismissing_a_row_reports_it_closed() {
        let mut centre = Centre::default();
        let mut store = store_with(&[(1, "slack"), (2, "mail")]);

        assert_eq!(
            centre.update(Message::Dismiss(1), &mut store, None),
            Action::Closed(vec![1])
        );
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn dismissing_a_row_that_is_already_gone_reports_nothing() {
        let mut centre = Centre::default();
        let mut store = store_with(&[(1, "slack")]);

        assert_eq!(
            centre.update(Message::Dismiss(99), &mut store, None),
            Action::None,
            "no NotificationClosed is owed for an id the store never had"
        );
    }

    #[test]
    fn clear_all_reports_every_id_it_removed() {
        let mut centre = Centre::default();
        let mut store = store_with(&[(1, "slack"), (2, "mail")]);

        let action = centre.update(Message::ClearAll, &mut store, None);
        let Action::Closed(mut ids) = action else {
            panic!("clear-all must report the ids it closed, got {action:?}");
        };
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
        assert!(store.history().is_empty());
    }

    #[test]
    fn clear_all_on_an_empty_centre_reports_nothing() {
        let mut centre = Centre::default();
        let mut store = Store::new();

        assert_eq!(
            centre.update(Message::ClearAll, &mut store, None),
            Action::None
        );
    }

    #[test]
    fn the_dnd_toggle_is_reported_up_rather_than_applied_here() {
        let mut centre = Centre::default();
        let mut store = Store::new();

        assert_eq!(
            centre.update(Message::SetDnd(true), &mut store, None),
            Action::Dnd(true)
        );
    }

    #[test]
    fn escape_closes_an_open_centre() {
        let mut centre = Centre::default();
        let mut store = Store::new();
        centre.set_open(true);

        assert_eq!(
            centre.update(Message::EscapePressed, &mut store, None),
            Action::Close
        );
        assert!(!centre.is_open());
    }

    #[test]
    fn escape_on_a_closed_centre_does_nothing() {
        let mut centre = Centre::default();
        let mut store = Store::new();

        assert_eq!(
            centre.update(Message::EscapePressed, &mut store, None),
            Action::None
        );
    }

    #[test]
    fn losing_focus_on_the_centres_own_surface_closes_it() {
        let mut centre = Centre::default();
        let mut store = Store::new();
        let surface = window::Id::unique();
        centre.set_open(true);

        assert_eq!(
            centre.update(Message::FocusLost(surface), &mut store, Some(surface)),
            Action::Close
        );
        assert!(!centre.is_open());
    }

    #[test]
    fn losing_focus_on_another_surface_leaves_the_centre_open() {
        let mut centre = Centre::default();
        let mut store = Store::new();
        let centre_surface = window::Id::unique();
        let stale = window::Id::unique();
        centre.set_open(true);

        assert_eq!(
            centre.update(Message::FocusLost(stale), &mut store, Some(centre_surface)),
            Action::None,
            "the surface a respawn just destroyed must not close the one that replaced it"
        );
        assert!(centre.is_open());
    }

    // ------------------------------------------------------------------
    // Action pills inside a history row
    // ------------------------------------------------------------------

    fn store_with_action(resident: bool) -> Store {
        let now = Instant::now();
        let mut store = Store::new();
        let mut notification = notification(1, "slack", now);
        notification.resident = resident;
        notification.actions = vec![StoreAction {
            key: "reply".to_string(),
            label: "Reply".to_string(),
        }];
        store.notify(notification, 0, true, now, &limits());
        store
    }

    #[test]
    fn a_pill_in_the_centre_invokes_and_closes() {
        let mut centre = Centre::default();
        let mut store = store_with_action(false);

        let action = centre.update(
            Message::Card(toast::Message::ActionClicked(1, "reply".to_string())),
            &mut store,
            None,
        );

        assert_eq!(
            action,
            Action::Invoked {
                id: 1,
                key: "reply".to_string(),
                closed: true
            }
        );
        assert!(store.history().is_empty(), "a non-resident row leaves");
    }

    #[test]
    fn a_pill_on_a_resident_row_invokes_and_stays() {
        let mut centre = Centre::default();
        let mut store = store_with_action(true);

        let action = centre.update(
            Message::Card(toast::Message::ActionClicked(1, "reply".to_string())),
            &mut store,
            None,
        );

        assert_eq!(
            action,
            Action::Invoked {
                id: 1,
                key: "reply".to_string(),
                closed: false
            }
        );
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn a_pill_for_a_row_that_no_longer_exists_invokes_nothing() {
        let mut centre = Centre::default();
        let mut store = Store::new();

        assert_eq!(
            centre.update(
                Message::Card(toast::Message::ActionClicked(7, "reply".to_string())),
                &mut store,
                None,
            ),
            Action::None
        );
    }
}
