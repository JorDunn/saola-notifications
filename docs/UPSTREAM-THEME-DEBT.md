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

_No gaps recorded yet — this file is seeded empty at Stage 1._

| Date | Gap | Local workaround | `saola-theme` session notified? |
| --- | --- | --- | --- |
