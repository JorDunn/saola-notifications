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
| 2026-08-28 | **No screen-edge inset token.** §6 anchors popovers, the notification centre and the toast stack "26px from the relevant edge"; `sizes.popover_top` carries the 72 px vertical half, but the horizontal 26 px only exists as `sizes.panel_margin_islands` — the right number under a name that is about the panel's islands. Wanted: a `sizes.shell_edge_gap` (or `popover_right`) the two can share. | `main.rs::toast_surface_settings` uses `sizes.panel_margin_islands` (26.0) for the toast surface's right margin, with a comment saying why. | No |
| 2026-08-28 | **No redraw-cadence token.** Every `motion.*` value is a *design* duration; an animated surface also needs a frame interval to repaint on, and each consumer is inventing its own (saola-capture's toast: 32 ms; its flash: 16 ms). Wanted: `motion.frame` (or similar) so animated Saola surfaces share one cadence. | `modules/toast.rs::REDRAW_INTERVAL` is 32 ms, saola-capture's own interim toast cadence carried over unchanged. | No |
| 2026-08-28 | **No easing helper.** §5 specifies the toast entrance as `ease-out`; `motion::fraction` is linear and `motion::toast_alpha` is built on it, so the theme's own §5 encoding is linear too. Wanted: either an easing function beside `fraction`, or a §5 correction saying the entrance is linear. | `modules/toast.rs::slide_offset` travels linearly, matching what `motion::toast_alpha` already does for the fade it plays alongside. Called out rather than fixed locally, because inventing a curve here would put the toast's motion out of step with every other Saola surface. | No |

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
one message rather than six. The message text is ready to paste; it is in
`.claude/handoffs/handoff_stage_5.attempt_1.md`.

None of these gaps are blocking: each has a token-only local workaround
already in place, and the two envelope functions are pinned to the theme's
own answers by test.
