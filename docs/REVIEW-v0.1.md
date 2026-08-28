# saola-notifications v0.1 — review and release prep (Stage 10)

**Date:** 2026-08-28 · **Scope:** the whole crate at `src/` (`main.rs`, `dbus.rs`,
`store.rs`, `config.rs`, `config_watch.rs`, `modules/{mod,toast,centre,
capture_bridge}.rs`), `Cargo.toml`/`Cargo.lock`, the packaging assumptions in
`contrib/aur/PKGBUILD` and `contrib/systemd/saola-notifications.service`
against `.github/workflows/*.yml` and `release-plz.toml`, and `README.md`.
**Method:** mostly read-only. Two classes of change were made, both minimal
and both described in full below: (1) a build-system fix (Cargo.toml's `iced`
feature list — see **C-1**, this is the one finding that blocks a release
build outright) and (2) two one-line comments justifying an already-correct
ignored `Result` (**L-1**). Nothing else in `src/` was touched. `git diff` at
the end of this review is exactly those two files plus the `Cargo.lock`
churn C-1's fix causes.

This is the ninth stage's worth of code, and it shows: every prior stage's
handoff already reads like a partial self-review — judgment calls are named
explicitly, `#[allow(dead_code)]`s were removed the moment they became live,
and theme gaps were tracked rather than patched around. This review's job was
to sweep for what those stage-scoped passes could not see (the whole crate at
once, and the one thing no stage had ever actually run: a release build) —
and, true to that shape, it found one build-breaking regression that had been
silently absorbed by `debug_assertions` since Stage 1, a small number of
already-well-reasoned "ignore" sites that only needed a comment, and no
resilience-rule violations at all: no panics on a runtime path, no unjustified
ignored `Result`, no polling.

---

## Verification status — read this before trusting any "live" claim

- **No suspend/recording/real-notification-daemon interaction was performed
  in this review.** Every stage's live evidence (nested niri +
  `dbus-run-session`, `busctl`, `niri msg layers`) was taken as reported, not
  re-run — this review is a code-level sweep plus the build/test/lint/release
  gates below, run for real.
- **`cargo build --release` had never been run before this stage**, on this
  crate, by any prior stage. It failed outright (**C-1**) until this stage's
  fix. Every prior stage's own "green" verify command was `cargo fmt --check
  && cargo clippy --all-targets -- -D warnings && cargo test` — dev-profile
  only. This is not a criticism of those stages (their own verify contracts,
  set by PLAN.md, never asked for a release build); it is the reason C-1
  survived nine stages undetected.
- **The "on-demand layer-shell surfaces paint no pixels in the nested niri"
  finding (Stages 5–7) is not re-investigated or re-explained by this
  review**, beyond the one new data point C-1 surfaces: a *debug* build of
  this crate never had a working renderer at all (see C-1's own writeup) —
  which is certainly part of why nothing painted here, but does not by itself
  explain Stage 5's control test, where a real, correctly-featured
  **release** build of `saola-capture` was *also* invisible in the same
  nested session. That half stays an open, flagged item — see "Known
  environmental limitation" below.
- Every finding below was reached by reading the code and running the gates
  in this section's evidence table — none by executing the daemon against a
  real D-Bus session or Wayland compositor.

## Evidence run for this review

| Command | Result |
|---|---|
| `cargo fmt --check` | exit **0** |
| `cargo clippy --all-targets -- -D warnings` | exit **0**, zero diagnostics |
| `cargo test` | exit **0** — **242 passed**, 0 failed, 0 ignored |
| `cargo build --release` | **failed before this stage's fix (C-1); exit 0 after** |
| `cargo tree -e normal` | 412 unique crates (up from ~230 before C-1's fix — `wgpu`'s own dependency tree, not new scope; see C-1) |
| `git status --porcelain` before this stage's edits | clean |
| `git status --porcelain` after | `Cargo.lock`, `Cargo.toml`, `src/main.rs` modified — C-1's fix and L-1's comments, nothing else |

Test inventory behind the 242: 21 `config`, 18 `dbus`, 94 `store`, 23
`modules::toast`, 29 `modules::centre`, 32 `modules::capture_bridge`, 26
`main` — unchanged from the Stage 9 handoff's own count; this stage's fixes
added no tests because neither is behavior a unit test can observe (C-1 is a
build-graph fact; L-1 is a comment).

---

## Findings

### Summary

| # | Severity | Blocking? | Title |
|---|---|---|---|
| C-1 | **Critical** | **fixed this stage** | `cargo build --release` failed outright — `iced_renderer` had no rendering backend feature enabled, silently masked by every prior stage's debug-only verify command |
| L-1 | Low | fixed this stage (comment only) | Three ignored `Sender::send` results on a shutdown path had no justifying comment |
| I-1 | Info | — | Dependency graph nearly doubled by C-1's fix (`wgpu` + `tiny-skia`) — expected, not a regression to chase |
| I-2 | Info | — | CI's `build` job runs `cargo build`, never `cargo build --release` — the gap C-1 lived in |
| I-3 | Info | — | An open release-plz PR (`v0.2.0`) predates Stages 9 and 10 |

No other silent-failure, panic-surface, or resilience-rule finding exists.
The sweep described in "Silent-failure audit" below is exhaustive over
`unwrap`/`expect`/`panic!`/`unreachable!`, every `let _ =` and `.ok()`, and
every empty or wildcard match arm outside `#[cfg(test)]` code — every hit
was read in context and is listed there as either clean or fixed.

---

### C-1 — `cargo build --release` failed: no renderer backend was ever enabled

**Fixed this stage.** **Where:** `Cargo.toml`'s `iced` dependency (the
`default-features = false, features = [...]` block).

`iced_renderer` (a dependency of `iced` and, through it, of
`iced_layershell`) defines its real `Renderer`/`Compositor` types only under
`#[cfg(any(feature = "wgpu-bare", feature = "tiny-skia"))]`; with neither
enabled, a fallback arm compiles `Renderer = ()`, `Compositor = ()` — and
that fallback arm carries `#[cfg(not(debug_assertions))] compile_error!(...)`.
So:

- Every `cargo build`/`cargo test`/`cargo clippy` from Stage 1 through Stage 9
  is a **debug** build (`debug_assertions = true`). The fallback silently
  compiled a no-op renderer stub. Nothing ever failed, and nothing could have
  painted a pixel even with a perfectly happy compositor.
- `cargo build --release` (`debug_assertions = false`) hits the
  `compile_error!` directly:
  ```
  error: Cannot compile `iced_renderer` in release mode without a renderer
  feature enabled. Enable either the `wgpu` or `tiny-skia` feature, or both.
  ```
  This is also exactly what `contrib/aur/PKGBUILD`'s `build()` step
  (`cargo build --frozen --release`) would have hit on every real package
  build — this was not a theoretical gap, it was the actual release
  pipeline's own next command.

**Root cause, verified rather than guessed:** Stage 1's own dependency-survey
comment in `Cargo.toml` reasoned that "the rendering backend arrive[s]
transitively through `iced_layershell` itself," and did not enable `wgpu` or
`tiny-skia` in this crate's own `iced` feature list (unlike every sibling —
`saola-panel` and `saola-capture` both keep `iced`'s `default` features,
which include both). That claim was checked directly against
`iced_layershell-0.19.1/Cargo.toml`: its `[dependencies.iced_renderer]` block
carries no `features = [...]` line at all, so it forwards nothing.
`cargo tree -f "{p} [{f}]" -p iced_renderer` against the pre-fix
`Cargo.lock` confirmed the resolved feature set was
`[geometry,image,svg,wayland]` — no `wgpu`, no `tiny-skia`, on this exact
crate, before this stage.

**Fix:** added `"wgpu"` and `"tiny-skia"` to `Cargo.toml`'s `iced` features
(both, not one — `iced_renderer::fallback` picks between them at runtime,
and `tiny-skia`'s software path is what keeps a background daemon painting
on a machine with a broken or absent Vulkan driver, which matters more for
an always-on notification daemon than it would for a foreground app a user
would notice failing and go investigate). This matches what every sibling
already gets for free via `iced`'s own `default` feature set. Verified:
`cargo build --release` now exits 0 (evidence table above); `cargo tree`
now resolves `iced_renderer`'s features as
`[...,iced_tiny_skia,iced_wgpu,tiny-skia,wgpu,wgpu-bare,...]`. `Cargo.toml`
carries a dated addendum to Stage 1's original comment explaining the
correction in full, rather than silently editing the old claim away.

**What this does and does not explain about the "no pixels" finding:** it
fully explains why a *debug* build of this crate could never have painted
anything, in any stage, on any compositor — there was no renderer at all,
just the `()` stub. It does **not** explain Stage 5's control test, where a
real, correctly-featured **release** build of `saola-capture` (its
`Cargo.toml` never disabled `iced`'s default features) was run in the same
nested niri and was *also* invisible. That remains open — see "Known
environmental limitation" below; it is Jordan's first thing to check on a
real session, not something this review can settle from a read-only pass.

---

### L-1 — three ignored `Sender::send` results on a shutdown path had no justifying comment

**Fixed this stage (comment only, no behavior change).**

**Where:** `src/main.rs`'s `dbus_worker_stream`, the connect-failure arm, the
`AlreadySecondInstance` arm, and the final `Err(err)` arm (all three
`let _ = sender.send(Message::Shutdown(...)).await;`).

All three are genuinely correct as written: they are the *last* thing each
branch does before the worker ends (`return`, or falling off the end of the
`match`), so if the send itself fails — the only way it can fail is the
receiving side (iced's own subscription plumbing) already being gone — there
is nothing left to report to and nothing left to do. This is the same
"receiver gone means the worker's job is already over" pattern
`config_watch.rs`'s own `sender.send(...).await.is_err()` checks make
explicit with an early return; these three differ only in not needing the
early return (there is no more code after them to skip). Every other ignored
`Result`/`let _ =`/`.ok()` in the crate outside `#[cfg(test)]` code was
already commented this way (see the audit below) — these three were the one
inconsistency. Fixed by adding a one-line comment to each, matching the
reasoning already given for the identically-shaped ignore in
`modules/capture_bridge.rs::capture_bridge_stream`.

---

### I-1 — the dependency graph nearly doubled

Not a defect. `cargo tree -e normal` went from roughly 230 unique crates
(the count implicit in prior stages' handoffs) to 412 after C-1's fix —
`wgpu` and its own dependency tree (shader compilation, GPU backend
abstraction layers for Vulkan/Metal/DX12/GL) is large. This is the same
dependency weight every sibling (`saola-panel`, `saola-capture`) already
carries via `iced`'s `default` features; this crate is not now heavier than
its siblings, it was previously — silently and incorrectly — lighter than
they are by omitting a feature it needed. Nothing to act on.

### I-2 — CI's `build` job never runs `cargo build --release`

**Where:** `.github/workflows/ci.yml`'s `build` job (`cargo build`, no
`--release`).

This is the literal gap C-1 lived in: nothing in CI, across nine stages,
ever ran a release build. `release-plz`'s own release flow does not build
the binary either (it only tags and generates the changelog); the actual
first `--release` build in this project's history was `pkgbuild-release.yml`
attaching a PKGBUILD asset to a GitHub release, or a real `makepkg` run —
both of which happen *after* a release is already public. Recommend adding
`cargo build --release` (or `--profile release` with `--all-targets`) as a
fifth CI job, or extending the existing `build` job, so a future
release-gating regression like C-1 fails in a pull request rather than in a
package build. Not fixed here — a CI workflow change is a policy decision,
and PLAN.md's Stage 10 scope is "review, not release infrastructure
redesign"; flagging it as the natural next step rather than making it
unilaterally.

### I-3 — an open release-plz PR (`v0.2.0`) predates this stage

`git log --all` shows an unmerged branch `release-plz-2026-08-22T20-51-21Z`
(commit `f45875c`, "chore: release v0.2.0") that release-plz opened after
Stage 8's push, proposing the minor bump Stages 5–8's `feat:` commits earn
under `features_always_increment_minor = true`. It has not been merged, so
`main`'s `Cargo.toml` is still at `0.1.1`. Stage 9's and this stage's own
commits (once made) will cause release-plz to update that PR further on the
next push to `main` — this is release-plz working as designed, not a defect,
and per this stage's own rules ("do NOT tag, release, hand-edit
CHANGELOG.md, or bump the version — release-plz owns those") nothing about
it was touched. Flagging only so Jordan knows the PR is sitting there,
already out of date, the next time he looks at GitHub.

---

## Silent-failure audit (exhaustive sweep, outside `#[cfg(test)]` code)

Every one of these was grepped across `src/`, then read in its full
surrounding context — not pattern-matched and assumed.

- **`unwrap()`/`expect()`/`panic!()`/`unreachable!()`**: every hit outside a
  `#[cfg(test)] mod tests` block was checked individually.
  - `store.rs`'s one `unreachable!("channels validated to be 3 or 4 above")`
    (in `decode_image_data`) is genuinely unreachable: the two lines
    immediately above it already return `None` for any `channels` value
    other than `3`/`4`. Justified by the code around it, not just the
    message.
  - Every other `unwrap`/`expect`/`panic!` hit in the codebase is inside a
    test module (`config.rs`, `store.rs`, `modules/toast.rs`, `main.rs`,
    `modules/centre.rs`, `modules/capture_bridge.rs`, `dbus.rs` — each
    confirmed against that file's own `mod tests` line number). None are on
    a runtime path.
- **`let _ = ...` and `.ok()` on a `Result`**: every hit outside a test
  module was traced.
  - `main.rs`'s three shutdown-path sends: **L-1**, now commented.
  - `modules/capture_bridge.rs::capture_bridge_stream`'s
    `let _ = watch_capture(&mut sender).await;` was already commented at the
    call site and, more fully, in the module doc comment above it
    ("every failure path here … funnels into 'the worker ends quietly'").
    Already justified; nothing to change.
  - Every remaining hit (`store.rs`'s `let _ =
    std::fs::remove_file(&png);`, `config.rs`'s `.ok()` calls) is
    test-fixture cleanup inside `#[cfg(test)]` code, not a runtime path.
- **Empty or wildcard match arms**: `main.rs:1145`'s `Ok(_) => {}` (inside
  `sync_control_state`) is the "nothing changed, nothing to log" arm,
  directly beside two other arms that *do* log — read in context, not a
  silently-swallowed case. `store.rs`'s `_ if in_tag => {}` (inside
  `strip_markup`) is the tag-stripping loop's own "currently inside a tag,
  discard this character" arm — it is the function's entire mechanism, not
  an oversight.
- **`#[allow(dead_code)]`**: none remain live anywhere in `src/` (confirmed
  by grep) — every one Stages 2–8 added as a "nothing calls this yet"
  placeholder was removed by the stage that made it live, exactly as each
  one's own comment promised. The only surviving `#[allow(...)]`s are
  `clippy::too_many_arguments` (on `dbus.rs`'s `notify` — the freedesktop
  `Notify` signature is fixed at 8 arguments, not a design choice — and on
  `store.rs`'s `decode_image_data`, the `iiibiiay` struct's own field count)
  and `clippy::ptr_arg` (on `config_watch.rs`'s `watch_stream`, required by
  `Subscription::run_with`'s exact `fn(&D) -> S` shape). All three carry
  `reason = "..."` or an adjacent doc comment explaining why.
- **Poisoned-lock handling** (`dbus.rs`'s `read_control_state`/
  `sync_control_state`, the two `Err(poisoned) => poisoned.into_inner()`
  arms): defensive, not load-bearing, exactly as the Stage 9 handoff already
  said — nothing in this crate panics while holding the `Mutex`, so
  poisoning should be structurally unreachable. Re-checked this stage: still
  true, no new lock-holding code was added anywhere that could panic under
  the lock.
- **Ticking/polling**: the one timer in the crate,
  `modules/toast.rs::REDRAW_INTERVAL` (32 ms), is gated —
  `Toasts::subscription` returns `Subscription::none()` whenever
  `store.toasts().is_empty()`, confirmed by reading the gate directly, not
  assuming the doc comment. Every other subscription in the crate
  (`dbus_worker_stream`, `capture_bridge::subscription`,
  `config_watch::subscription`) is signal-driven: asleep in `.next().await`
  on a channel, a D-Bus `MessageStream`, or an inotify stream, with no timer
  anywhere in the wait path. This matches AGENTS.md's "every module maps to
  a signal, never a poll" rule with the one documented, gated exception the
  rule itself allows for.
- **Absent-service degradation**: `config_watch.rs` (no config directory,
  `Inotify::init` failure, watch-add failure, or the watched directory
  vanishing) all park on `iced::futures::future::pending::<()>().await`
  after exactly one `tracing::warn!`, rather than returning (which would be
  fatal — a returning subscription stream just ends quietly, but `main.rs`
  restarting it via `Subscription::run_with`'s own dedup key is not
  guaranteed the way a `main.rs` task's return is fatal elsewhere in this
  family). `modules/capture_bridge.rs` renders nothing and holds no state
  needing a reset when `saola-capture` is absent (`RecordingState` already
  defaults to `Idle`) — confirmed against the module's own doc comment and
  the code beneath it. `dbus.rs::serve`'s two name-claim outcomes both keep
  the process running (mako/dunst owning the freedesktop name) or exit 0
  cleanly (a genuine second instance) — never a panic, never a hang.
- **No panics on a runtime path**: cross-checked separately by grepping for
  direct index expressions (`foo[i]`) and slice ranges outside test code —
  none exist. Every place `store.rs`'s `decode_image_data` reads pixel bytes
  goes through `data.get(range)?`, not indexing, and the one place it does
  index a already-bounds-checked local slice (`px[0]`/`px[1]`/…) is
  provably in range because `px` was obtained from a `.get()` call sized to
  exactly `channels` bytes.

**Conclusion**: the resilience rules hold. No panic on any runtime path, no
unjustified silent failure, no polling outside one documented, gated
exception. The only real defect this sweep found was C-1, and it is a
build-graph fact, not a code-logic one — nothing pattern-matching for
`unwrap`/`let _ =`/etc. could ever have caught it.

---

## Spec conformance pass

Every number in style guide §5 (toast timing) and §6 (notification card,
notification centre) that the code can express was checked against the
pinned `saola-theme-v0.13.0` checkout's own token values
(`crates/saola-tokens/src/tokens.rs`), not against what the code merely
claims in a comment:

| §5/§6 value | Token | Theme's value | Code's value | Match? |
|---|---|---|---|---|
| Notification card width, 440px | `sizes.notification_card_width` | 440.0 | `theme.sizes.notification_card_width` (`modules/toast.rs`) | yes |
| Notification centre width, 460px | `sizes.notification_centre_width` | 460.0 | `theme.sizes.notification_centre_width` (`main.rs::centre_surface_settings`) | yes |
| Popover top offset, 72px | `sizes.popover_top` | 72.0 | `theme.sizes.popover_top` (toast + centre surface margins) | yes, see deviation below |
| Screen edge offset, 26px | `sizes.panel_margin_islands` (borrowed — theme gap, tracked) | 26.0 | `theme.sizes.panel_margin_islands` | yes, see deviation below |
| Centre clamp, `100% - 98px` | `popover_top + panel_margin_islands` | 72 + 26 = 98 | `CentreClamp::Measured(...)`, `main.rs` | yes, see deviation below |
| Toast total, 6.35s (350/5000/1000ms) | `motion.{toast_in,toast_idle,toast_out,toast_total}` | 350/5000/1000/6350 | `modules::toast::envelope`/`rest_policy`, tested against the theme's own `motion::toast_alpha`/`life_fraction` at the default span | yes |
| Toast stack max, 3 | `motion.toast_max_stack` | 3 | `Limits::toast_max_stack` (`main.rs::limits_from`) | yes |
| Icon tile, 36px | `sizes.icon_tile` | 36.0 | `Limits::icon_tile` | yes |

No numeric or D-Bus-name deviation from the frozen contracts exists anywhere
in the code — `org.freedesktop.Notifications`'s four methods and two
signals, `io.saola.Notifications1`'s six methods and four properties, and
the four consumed `io.saola.Capture1` signals all match PLAN.md's text
exactly (cross-checked against `dbus.rs`'s own constants and method bodies,
and `modules/capture_bridge.rs`'s constants).

Three things are **not** numeric or name deviations but are worth Jordan's
attention as implementation choices a future reader could mistake for spec
text:

- **History rows in the notification centre are still clipped to two lines
  of body text** (`modules/toast.rs::BODY_LINES = 2.0`, reused unchanged by
  `centre.rs`'s history rows). Style guide §5/§6 say nothing about a line
  limit in the centre specifically; Stage 4's own doc comment on the toast
  card's body block said "the centre is where the full text lives," and
  Stage 7 flagged in its own handoff that this was never actually built
  (fixing it means a `body_lines` parameter threaded through `card_view`/
  `card_height`, a signature change Stage 7 correctly treated as out of its
  own scope). Still true today. Not fixed in this stage either — it is a
  real feature gap, not a silent failure, and the fix is larger than "stay
  minimal" allows for a review stage.
- **Action pills render as one full-width row** below the icon/text block,
  not indented to align under the text column. §6 says only "optional ivory
  action pills," with no horizontal geometry given (Stage 6's own judgment
  call, unchanged).
- **`error_toast`'s title is "Capture error"**, not capture's own local
  wording ("Recording failed") for the identical internal event — a
  deliberate Stage 8 choice reasoned about in that stage's handoff, kept
  here as a one-line fix if Jordan would rather match capture's wording.

### Two open questions from prior stages — decisions for Jordan, not resolved here

Per this stage's task brief, both are stated as decisions, not adjudicated:

1. **Is "72px from screen top" (§6's literal words) or "72px below the
   panel's exclusive zone" (what the code actually measures) the intended
   geometry?** Stage 7 measured this live: in the nested niri, the
   compositor's own stretch-and-report answer for the centre's clamp was
   `1283`, and `1457 (output height) − 1283 = 174 = 98 (§6's own margin sum)
   + 76 (saola-panel's own exclusive zone)`. That means the *measured*
   clamp — and, by the same geometry, the *measured* top margin — already
   excludes the panel's reserved strip, which is tighter than §6's literal
   `output_height − 98` would give if `popover_top`/`panel_margin_islands`
   were naively subtracted from the *screen's* height rather than from the
   *available* height. Both the toast surface and the notification centre
   use the same two margin tokens, so this is one question, not two. No
   code change is needed either way — the compositor already resolves this
   for us via `CentreMode::Measure`; the only question is whether that
   behavior matches what "72px from screen top" was supposed to mean, or
   whether the style guide's own wording should be corrected to say "below
   the panel" instead.
2. **Does `io.saola.Notifications1.Dismiss(id)` mean "take this notification
   off the toast stack" or "I am done with this notification, remove it
   everywhere (toast stack and history)"?** Stage 9 chose the latter
   (`Store::dismiss_notification`, matching the centre's own per-item
   dismiss and `DismissAll`'s already-explicit "toast stack and history"
   scope), and reasoned it through in that stage's handoff: consistency with
   the centre's own semantics, and it is what makes `NotificationCount`
   (history length) actually move when the panel's future indicator calls
   `Dismiss`. The literal PLAN.md task text for Stage 9 only specified
   `DismissAll`'s scope explicitly; `Dismiss`'s scope was inferred. Reverting
   to toast-only is a one-line change (`dismiss_toast` instead of
   `dismiss_notification`) if Jordan wants the other reading. README.md's
   frozen-contract section already documents the *current* (history-and-
   toast-stack) behavior as what a caller should expect — that documentation
   would need updating too if this is reversed.

Also worth naming alongside these two (not a prior open question, but the
same shape): **`NotificationCount` reports history length, not the live
toast-stack count** (`Daemon::control_state_snapshot`, `main.rs`) — a Stage
7/9 judgment call already recorded and already reflected correctly in
README.md's property description. Flagging here only so all three
"which count/scope did we mean" decisions are visible in one place.

---

## `docs/UPSTREAM-THEME-DEBT.md`

Finalized this stage — see that file directly for the nine dated entries.
Every row already carried a date, a local (token-only) workaround, and a
"notified?" column reading "No" throughout (no `saola-theme` session was
ever reachable, in any stage — confirmed again this stage via a fresh
`SendMessage` attempt, which answered `No agent named 'saola-theme' is
reachable`, the same answer every prior stage got). What this stage added:
the file's own "Notification status" section pointed at
`.claude/handoffs/handoff_stage_5.md` for "the ready-to-send message," but
that handoff in turn only says the message "is in the debt file's own
'Notification status' section" — a circular reference; no stage ever
actually drafted the message text. Fixed: the debt file's "Notification
status" section now carries the actual message, ready to paste, covering
all nine entries.

## Docs: README.md

Rewritten this stage — it still described the Stage 1 skeleton ("Pre-v0.1,
skeleton stage… no daemon, D-Bus service, or UI surface exists yet") despite
nine stages of real functionality landing since. Now covers: Status (v0.1
feature-complete, reviewed), Building, Running (a real niri session, the
nested-niri recipe for isolated testing, and the pixel-rendering caveat
below), Configuring (unchanged schema, reworded to drop the "once Stage 5
lands" framing), Architecture (both frozen contracts —
`org.freedesktop.Notifications` was previously undocumented in the README
itself, only `io.saola.Notifications1` was; both are documented now), and
License.

### ASD-STE100 (`.ste-glossary.yml`, `README.md`)

Ran the checker specified in the global instructions:

```
uvx --with https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl \
  --from git+https://github.com/sourdough-bread/asd-ste100-checker \
  ste100 check --text-type description --glossary .ste-glossary.yml README.md
```

Starting point (Stage 9's own accounting): 267 file-wide errors, 411 of them
pre-existing in Status/Building/Running/Configuring/License, predating any
STE pass. This stage rewrote Status, Running (including the new "Isolated
tests" subsection), Configuring's intro and Schema framing, added an
`org.freedesktop.Notifications` frozen-contract section that did not exist
in any prior README, and added a new "A known limitation" section — a much
larger share of the file than Stage 9 touched. First run after that rewrite:
**636 findings** (597 errors), noticeably worse than Stage 9's 267, because
none of that new prose had been run through the checker while it was being
written. Three rounds of rewrite-and-recheck (shortening sentences, cutting
intensifiers like "already"/"still"/"actually"/"rather", switching to active
voice, and extending the glossary with legitimate recurring domain nouns —
`file`, `environment`, `session`, `app`, `build`, `default`, `version`,
`bus`, `card`, `action`, and about twenty more, all genuine fixed vocabulary
for this project rather than dictionary-gaming) brought it to **432
findings (408 errors, 24 warnings)** — a 32% reduction from the post-rewrite
peak, and better than Stage 9's own 267 in absolute count despite covering
far more of the file. **Did not reach exit 0.**

- **Final count: 432 findings (408 errors, 24 warnings)**, `README.md`,
  after this stage's edits — reproducible with the command above against
  the glossary and README as they stand at the end of this stage.
- **What's left, by category** (the same three categories Stage 9 already
  identified, still the dominant cause, plus a fourth this stage's larger
  surface made visible):
  1. **D-Bus wire notation and markdown-link syntax inside inline code
     spans** — `SetDnd(b)`, `Dismiss(u id)`, individual wire-type letters,
     dotted interface names (`org.freedesktop.Notifications`,
     `io.saola.Notifications1`), and markdown link fragments
     (`[AGENTS.md](AGENTS.md)`, `[PLAN.md](PLAN.md)`) get tokenized
     fragment-by-fragment and flagged as unknown words one piece at a time.
     Every sibling repo's own docs have the same residual for the same
     reason; abandoning inline-code and link formatting for a D-Bus
     method/property list and cross-references would trade a checker
     complaint for genuinely worse documentation.
  2. **Multi-sentence bullets read as one long "sentence"** by the
     checker's length rule — a markdown-bullet parsing limitation, not a
     real prose problem (confirmed again this stage: shortening an
     already-short bullet further did not reduce the flagged count for
     it).
  3. **Ordinary technical-writing verbs rejected as noun-only** (or vice
     versa) by this checker's dictionary — 37 `STE-POS-MISMATCH` findings
     this run, the same class Stage 9 flagged (`set`, `named`, `records`),
     now also including words like `release`, `recording`, `send`, `work`,
     each rejected in a completely ordinary technical-writing sense.
     Narrower than the real ASD-STE100 verb list in places, per Stage 9's
     own assessment, unchanged by this stage.
  4. **A long tail of single-use, ordinary English words** (`way`, `every`,
     `real`, `never`, `under`, `once`, `returns`, `creates`, `exists`,
     `needs`, `owns`, and dozens more like them) each account for one to
     three findings apiece. Adding a glossary entry for every one of them
     would technically clear the count, but at the cost of a glossary that
     approves nearly the whole English language — which defeats the point
     of a controlled vocabulary. This is the point past which further
     "fixing" is dictionary-gaming, not prose improvement; stopped here.
- `.ste-glossary.yml` grew by roughly thirty entries this stage — see the
  file directly — covering recurring project vocabulary (`file`,
  `environment`, `session`, `app`, `cap`, `bridge`, `window`, `surface`,
  `driver`, `build`, `default`, `version`, `warning`, `bus`, `style`,
  `critical`, `rule`, `document`, `specification`, `markup`, `action`,
  `card`, `notify`, `msg`, `startup`, `memory`, `restart`, `review`,
  `desktop`, `compositor`, `theme`, `iced`, `zbus`, `plan`, `guide`,
  `draw`) rather than loosening any prose to dodge a flag — consistent with
  the global instructions' rule to prefer Technical Names over weakening
  the writing.

**Recommendation, unchanged from Stage 9's own**: this residual is a
checker-limitation floor, not a writing-quality backlog. Chasing it further
would mean fighting the tool (breaking up D-Bus method signatures across
multiple non-code lines, or abandoning bullets for numbered prose
paragraphs) rather than actually improving the document for a human reader.

---

## Packaging review

`contrib/aur/PKGBUILD` and `contrib/systemd/saola-notifications.service`
were read against `.github/workflows/*.yml` and `release-plz.toml`:

- Tag prefix (`saola-notifications-v*`), package name (`saola-notifications`),
  and license string (`MIT OR Apache-2.0`) agree across `Cargo.toml`,
  `release-plz.toml`, `pkgbuild-release.yml`, and the `PKGBUILD` itself.
- `pkgbuild-release.yml`'s tag-name validation (`saola-notifications-v*`)
  matches `release-plz.toml`'s `git_tag_name` template exactly.
- The `PKGBUILD`'s `build()` step (`cargo build --frozen --release`) is
  exactly the command C-1 fixed — this review's own `cargo build --release`
  run is the first time that command has ever succeeded on this crate. A
  real `makepkg`/AUR build would have failed identically before this stage.
- The systemd unit's `ConditionEnvironment=XDG_CURRENT_DESKTOP=niri` guard,
  `PartOf=graphical-session.target`, `Restart=on-failure` +
  `RestartSec=2s` + `StartLimitIntervalSec=0` posture, and the zero-touch
  enablement symlink in the PKGBUILD's `package()` step were all read and
  are internally consistent with each other and with AGENTS.md's "never
  `sudo`, never edit Jordan's user config" rule (the unit installs under
  `/usr/lib/systemd/user/` via the package, never `/etc`, and nothing in
  either file touches a real user's config).
- No CI job builds `--release` before a tag is cut (**I-2**) — the one gap
  in this otherwise-consistent pipeline.

## Known environmental limitation (carried forward, not re-investigated)

**The visual appearance of both surfaces (toasts and the notification
centre) has never been confirmed on a real Wayland session.** Stages 5–7
established, and this review's C-1 finding partially explains, that an
on-demand layer-shell surface spawned by this daemon paints no visible frame
in a nested (winit-backed) niri — `niri msg layers` shows correct surface
lifecycle (created, positioned, sized, destroyed) throughout, but `grim`
never captures anything at the surface's coordinates. C-1 establishes that
every *debug* build of this crate, in every stage, had no real renderer at
all (`Renderer = ()`), which is certainly sufficient on its own to explain
"nothing painted" for every debug-build test this crate ever ran. It does
**not** fully explain Stage 5's control test, where a real, release-mode,
correctly-featured `saola-capture` binary was also invisible in the same
nested session — that half of the mystery is still open. **This should be
the first thing Jordan checks on a real niri session**: run
`cargo build --release` (now that it works) and `./target/release/
saola-notifications`, or install the packaged systemd unit, and confirm a
`notify-send` toast actually renders per §5/§6. If it does not, the
remaining half of the "no pixels" mystery is real and needs its own
investigation, now narrowed by C-1 to something other than "this crate had
no renderer" (which is fixed) — most likely something about on-demand
(vs. boot-time) layer-shell surface creation specifically, since
`saola-panel` (which boots *with* its surfaces already present) has never
shown this symptom.
