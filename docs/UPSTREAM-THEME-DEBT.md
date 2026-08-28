# Upstream saola-theme debt

Gaps between what `saola-notifications` needs from `saola-theme` and what
the pinned tag currently ships. AGENTS.md's theme-gap protocol forbids
patching a missing style or token locally ("zero hardcoded colors or
sizes… if a style is missing, add it to saola-theme — never restyle
locally"), so each gap is recorded here instead: a dated entry, the local
workaround in place until the token lands upstream, and whether the open
`saola-theme` session was notified.

None of these should block a `saola-notifications` release — they exist so
a `saola-theme` release can pick them up deliberately, not rediscover them
as regressions.

Pinned tag at the time of writing: `saola-theme-v0.13.0`.

| Date | Gap | Local workaround | `saola-theme` session notified? |
| --- | --- | --- | --- |
| 2026-08-28 | **No urgent *notification* card.** `style::container::card_urgent(t, s)` is `card` plus the accent ring, and `card`'s `Surface::Ink` arm paints an **ivory** card with ink text — style guide §6's notification card is solid **ink** with ivory text. It also takes no `alpha`, so it cannot fade with the rest of the toast. Wanted: `style::container::notification_card_urgent(t, alpha)` — `notification_card`'s recipe plus `style::accent_ring`. | `modules/toast.rs::urgent_card_style` composes `notification_card(t, alpha)` with `style::accent_ring(t, radii.card)` and scales the ring's own alpha. Every value is still a token. | No — see "Notification status" below |
| 2026-08-28 | **`style::notification::{life_rule, icon_tile}` take no `alpha`**, unlike `style::container::notification_card(t, alpha)`, so a card's two inner chrome pieces stay fully opaque while the card around them fades (iced 0.14 has no subtree opacity — the alpha has to reach every painted color). | `modules/toast.rs::{life_rule_style, tile_style}` reproduce the two helpers' exact recipes (`on_ink.fill_subtle`, `palette.accent`, `on_ink.primary`, `radii.tile`, `style::border_none`) with `ColorExt::with_opacity(alpha)` applied. | No |
| 2026-08-28 | **`motion::toast_alpha` / `motion::life_fraction` hardwire `motion.toast_idle` as the rest span.** A `Notify` call carries its own `expire_timeout`, which PLAN.md and §5 make the rest phase's length, so neither helper can express a non-default toast. Wanted: a variant taking the rest span (`toast_alpha_over(t, rest, elapsed)`), with the current functions as the `rest = motion.toast_idle` case. | `modules/toast.rs::{card_alpha, life_fraction}` generalize the same envelope over a rest span, built from `motion::fraction` and the `motion.toast_*` tokens. Two tests (`*_matches_the_theme_for_a_default_rest_span`) assert they agree with the theme's own functions at the default span, so the local copies cannot drift off §5. | No |
| 2026-08-28 | **No screen-edge inset token.** §6 anchors popovers, the notification centre and the toast stack "26px from the relevant edge"; `sizes.popover_top` carries the 72 px vertical half, but the horizontal 26 px only exists as `sizes.panel_margin_islands` — the right number under a name that is about the panel's islands. Wanted: a `sizes.shell_edge_gap` (or `popover_right`) the two can share. | `main.rs::toast_surface_settings` uses `sizes.panel_margin_islands` (26.0) for the toast surface's right margin, with a comment saying why. Stage 7's `main.rs::centre_surface_settings` borrows the same token for the notification centre's right **and** bottom margin, so the number now stands in three places under a name about none of them. | No |
| 2026-08-28 | **No redraw-cadence token.** Every `motion.*` value is a *design* duration; an animated surface also needs a frame interval to repaint on, and each consumer is inventing its own (saola-capture's toast: 32 ms; its flash: 16 ms). Wanted: `motion.frame` (or similar) so animated Saola surfaces share one cadence. | `modules/toast.rs::REDRAW_INTERVAL` is 32 ms, saola-capture's own interim toast cadence carried over unchanged. | No |
| 2026-08-28 | **No easing helper.** §5 specifies the toast entrance as `ease-out`; `motion::fraction` is linear and `motion::toast_alpha` is built on it, so the theme's own §5 encoding is linear too. Wanted: either an easing function beside `fraction`, or a §5 correction saying the entrance is linear. | `modules/toast.rs::slide_offset` travels linearly, matching what `motion::toast_alpha` already does for the fade it plays alongside. Called out rather than fixed locally, because inventing a curve here would put the toast's motion out of step with every other Saola surface. | No |
| 2026-08-28 | **No alpha-aware action-pill style.** Style guide §6's "optional ivory action pills" match `style::button::rest`/`emphasis` at `(Surface::Ink, Chrome::Shell)` exactly (a solid ivory pill, ink label — `widget::pill_button` already builds this pill's whole geometry), but neither the style helpers nor `widget::pill_button` take an `alpha` — the same gap Stage 5 already logged against `notification::{life_rule, icon_tile}`, now on a third widget family. A toast's pills can't fade in step with the rest of the card (iced 0.14 has no subtree opacity) without one. Wanted: an `alpha` parameter on `style::button::rest`/`emphasis` (mirroring `container::notification_card`'s), or a faded `widget::pill_button` variant. | `modules/toast.rs::action_pill`/`action_pill_style` reproduce `widget::pill_button`'s exact recipe (`sizes.hit_target_bar` height, `paddings.pill_button` padding, `ui_font` at `typography.size.body`, `widget::centered`) by hand, wrapping `style::button::rest(t, Surface::Ink, Chrome::Shell)` and scaling the returned `Style`'s background/text-color alpha afterward — the same pattern Stage 5's `tile_style`/`life_rule_style` already established for this exact class of gap. | No — see "Notification status" below |
| 2026-08-28 | **`widget::empty_state` is unconditionally `Fill` × `Fill`.** Every surface in this crate is a layer-shell surface, and a layer-shell surface's height is declared *before* iced measures anything — so the notification centre computes its own height from tokens and has no `Fill` to give away. Dropped in as-is, the empty state resolves to its minimum inside a shrink-height column and the "No notifications" line reads as a stray label rather than a band. Wanted: a `height` parameter, or an `empty_state_row` that honours `sizes.list_row` the way `widget::quiet_row` does. | `modules/centre.rs::Centre::view` wraps `widget::empty_state(t, Surface::Ink, …)` in a `container(...).height(sizes.list_row)`, and `centre_height` budgets exactly that one row. Both terms are tokens; nothing is invented. | No — see "Notification status" below |
| 2026-08-28 | **Nothing carries the notification centre's *vertical* rhythm.** `sizes.notification_centre_width` (460) is the only centre token; §6's "hugs its content" arithmetic — the chrome band, the gap under it, the card inset, the clear-all row's height — is assembled by the consumer from generic tokens. That is workable, but it means a second consumer of the same shape (a saola-panel indicator popover, a settings preview) will reinvent the spacing rather than share it, and the two will drift. Wanted: either centre-specific `sizes.*`, or a documented statement in §6 that the rhythm is `popover_padding` / `list_row` / `island_gap` / `hit_target_bar`, which is what this crate chose. | `modules/centre.rs::centre_height` is built from `sizes.popover_padding`, `sizes.list_row`, `sizes.island_gap`, `sizes.gap_tight` and `sizes.hit_target_bar`, with the card heights coming from `modules/toast.rs::card_height`. The choice of the card inset is forced rather than invented: 460 − 440 is exactly two `sizes.island_gap`s. Six unit tests pin the arithmetic. | No |

## Notification status

The theme-gap protocol's step 2 is "notify Jordan's open `saola-theme`
session via SendMessage (find it with ListAgents)". At Stage 5 (2026-08-28)
**no such session was open**: `SendMessage` to `saola-theme` was attempted
and answered `No agent named 'saola-theme' is reachable`. (`ListAgents` was
not in the Stage 5 agent's toolset either, so the send itself was the
discovery.)

Every entry above is therefore recorded as "No", and announcing them
upstream is an open item — carried in the Stage 5 handoff so that whoever
next has a `saola-theme` session open (or Jordan directly) can send them in
one message rather than six.

**Stage 6 (2026-08-28)** added the seventh row above (the alpha-aware
action-pill style) under the same constraint: no `saola-theme` session was
reachable from this stage either (the task briefing states this directly —
no `SendMessage`/`ListAgents` attempt was available to retry). The new
entry is folded into the same "not yet sent" backlog rather than opening a
second one; whoever next has a `saola-theme` session open can send all
seven gaps in one message.

**Stage 7 (2026-08-28)** added the eighth and ninth rows above (the
`Fill`-only empty state, and the missing centre rhythm) and extended the
screen-edge-inset row, which the notification centre now borrows from as
well. The Stage 7 task briefing again states directly that **no
`saola-theme` session exists**, so the backlog is unchanged in kind: nine
gaps, none sent.

Stage 7 also found one gap that is **not** saola-theme's and is recorded
here only so it is not looked for in the wrong repo: `iced_layershell` 0.19
exposes no output geometry to an application, so §6's `calc(100% - 98px)`
has no `100%` to read. `main.rs` measures it from the compositor instead
(see `CentreMode::Measure`); no theme token would help.

**Stage 10 (2026-08-28, release/review prep)** tried again — `SendMessage`
to `saola-theme` still answers "No agent named 'saola-theme' is reachable."
Nine gaps, still none sent, four stages running. This stage also found and
fixed a broken pointer: prior stages each said the ready-to-paste message
text lived somewhere else (the Stage 5 handoff said it was in this file; a
prior draft of this file said it was in the Stage 5 handoff) — nobody had
actually written it. It is below now, not pointed at.

None of these gaps are blocking: each has a token-only local workaround
already in place, and the two envelope functions are pinned to the theme's
own answers by test.

### Ready-to-send message

Paste this to a `saola-theme` session the moment one is reachable
(`SendMessage`, found via `ListAgents`):

> `saola-notifications` v0.1 carries nine recorded gaps against
> `saola-theme-v0.13.0` (the pinned tag), each with a token-only local
> workaround already shipping — none of these block a `saola-notifications`
> release, they're here so a `saola-theme` release can pick them up
> deliberately. Full detail and exact call-site references are in
> `saola-notifications/docs/UPSTREAM-THEME-DEBT.md`; short version:
>
> 1. No urgent *notification* card — `style::container::card_urgent` paints
>    an ivory card with ink text (§6's notification card is solid ink with
>    ivory text) and takes no `alpha`. Wanted: `notification_card_urgent(t,
>    alpha)`.
> 2. `style::notification::{life_rule, icon_tile}` take no `alpha`, so a
>    card's inner chrome can't fade with the rest of it (iced 0.14 has no
>    subtree opacity).
> 3. `motion::{toast_alpha, life_fraction}` hardwire `motion.toast_idle` as
>    the rest span — can't express a `Notify` call's own `expire_timeout`.
>    Wanted: a variant taking the rest span as a parameter.
> 4. No shared screen-edge-inset token — `sizes.panel_margin_islands` (the
>    panel's own islands margin) now stands in for a generic "26px from the
>    relevant edge" in three unrelated places (toast surface, notification
>    centre, and originally the panel itself).
> 5. No redraw-cadence token — every animated Saola surface invents its own
>    frame interval (32ms here, matching saola-capture's own toast).
> 6. No easing helper — §5 specifies `ease-out` for the toast entrance;
>    `motion::fraction` is linear, so the entrance is linear too.
> 7. No alpha-aware action-pill style — `style::button::rest`/`emphasis`/
>    `widget::pill_button` take no `alpha`, same class of gap as #2, on a
>    third widget family.
> 8. `widget::empty_state` is unconditionally `Fill × Fill`, which no
>    layer-shell surface (height declared before layout) can give it.
> 9. Nothing carries the notification centre's *vertical* rhythm —
>    `sizes.notification_centre_width` is the only centre-specific token;
>    the rest is assembled from generic tokens and will drift if a second
>    consumer reinvents it differently.
>
> No action needed beyond acknowledging receipt if you're picking any of
> these up — every workaround already in place is token-only, so there's no
> urgency, just a backlog worth clearing when saola-theme next has room.
