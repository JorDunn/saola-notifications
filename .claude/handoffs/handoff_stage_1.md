# Stage 1 handoff — repo skeleton and dependency survey

Status: DONE. Verify command green locally (see Stage 2's own verify command
below for the same environment — it already resolves and compiles clean).

## Files created

- `Cargo.toml` — edition 2024, binary crate `saola-notifications`,
  `license = "MIT OR Apache-2.0"`, version `0.1.0`. Every dependency carries
  a dated (2026-08-17) survey comment.
- `src/main.rs` — stub: `init_tracing()` (lifted from
  `saola-session::main::init_tracing`, defaults to `"info"`, `RUST_LOG`
  override via `env-filter`) then one `tracing::info!` and return. No
  layershell, no D-Bus, no config — intentionally, per scope.
- `LICENSE-MIT`, `LICENSE-APACHE` — byte-copied from `saola-capture`. The
  old single `LICENSE` (Apache-only) was deleted.
- `README.md` — family template (H1, Status, Building, Running,
  Configuring, Schema, Architecture for contributors, License). Schema
  section documents the Stage 2 `notifications.toml` shape (`dnd-default`,
  `history-cap`, `critical-bypasses-dnd`, reserved `[apps]`) as *planned*,
  not yet read by the binary — update the "planned" framing once Stage 2
  lands config.rs.
- `CHANGELOG.md` — Keep a Changelog 1.1.0 header + empty `## [Unreleased]`.
- `rustfmt.toml`, `rust-toolchain.toml` — byte-copied from `saola-files`
  (default rustfmt config; toolchain pinned to `stable`).
- `release-plz.toml` — copied from `saola-files`, `saola-files-v*` swapped
  for `saola-notifications-v*` in the header comment (the actual
  `git_tag_name` template `"{{ package }}-v{{ version }}"` is
  package-name-driven already, no edit needed there).
- `.github/workflows/ci.yml` — copied from `saola-capture` (closer match
  than `saola-files`: a single binary crate with no feature matrix, not the
  all-features/no-default-features double build `saola-files` needs for its
  protocol-module features). Jobs: fmt, clippy (`-D warnings`, no
  `--all-features`), test, build. `APT_BUILD_DEPS=libxkbcommon-dev
  libwayland-dev`.
- `.github/workflows/release-plz.yml`, `.github/workflows/pkgbuild-release.yml`
  — copied from `saola-files`, `saola-files`/`saola-files-v*` swapped for
  `saola-notifications`/`saola-notifications-v*` throughout (branch names,
  tag prefixes, error messages, `pkgdesc` references).
- `contrib/aur/PKGBUILD` — modeled on `saola-session`'s (closer fit than
  `saola-files`': ships a systemd unit + zero-touch enablement symlink, which
  this repo also does; `saola-files` has no systemd unit). `depends=(gcc-libs
  glibc libxkbcommon wayland vulkan-icd-loader)` — the standard
  iced+iced_layershell wgpu-renderer runtime deps, cross-checked against
  `saola-panel`'s and `saola-lockscreen`'s own `depends=` lines (both use the
  same iced/iced_layershell stack). `makedepends=(cargo git)` — git because
  `saola-theme` is a git dependency.
- `contrib/systemd/saola-notifications.service` — modeled on
  `saola-session`'s unit shape but trimmed to exactly what the Stage 1 task
  specified: `After=graphical-session.target` (not
  `...target niri.service` — session's daemon needs the compositor socket
  directly for Wayland idle-notify; this daemon's Stage-1 stub does not, so
  the extra ordering dependency was not carried over — revisit if a later
  stage's D-Bus/layershell work needs it), `PartOf=graphical-session.target`,
  `ConditionEnvironment=XDG_CURRENT_DESKTOP=niri`, `Restart=on-failure`,
  `RestartSec=2s`, `StartLimitIntervalSec=0`.
- `docs/SAOLA-STYLE-GUIDE.md` — vendored byte-identical from
  `/home/jordan/Developer/saola-theme/design/SAOLA-STYLE-GUIDE.md` (`diff`
  confirmed identical after copy). **Note:** this is newer/longer (554
  lines) than the copy already vendored in `saola-capture/docs/` (535
  lines) — saola-theme has moved on since capture's copy was taken (extra
  "Controls inside an ink window" section, dated 2026-08-13). Re-vendor from
  `saola-theme` directly again whenever the pinned tag bumps, not from a
  sibling's stale copy.
- `docs/UPSTREAM-THEME-DEBT.md` — seeded empty (format lifted from
  `saola-files/docs/UPSTREAM-THEME-DEBT.md`'s intro prose + table, table
  itself has zero rows).

## Files verified, NOT edited

- `AGENTS.md`, `CLAUDE.md` — both already existed and were read carefully
  against `PLAN.md`'s Context/Architecture/Frozen-external-contracts
  sections line by line (surface geometry, D-Bus bridge shape, name-claim
  posture, module pattern, resilience rules, theme-gap protocol, DND policy,
  Boundaries/roadmap). No inaccuracies or gaps found — left untouched.
  `CLAUDE.md` is the one-line `@AGENTS.md` convention, confirmed correct.

## Key decisions

- **`image` crate: YES, added directly.** Verified (downloaded and read
  `iced-0.14.0/Cargo.toml` and `image-0.25.10/Cargo.toml` directly, not
  assumed): iced's `image` feature = `image-without-codecs` +
  `image/default` (which pulls *every* format codec — avif/bmp/dds/exr/ff/
  gif/hdr/ico/jpeg/png/pnm/qoi/tga/tiff/webp). `image-without-codecs` alone
  gives the `image::Handle`/viewer widget machinery but a codec-less
  `image` dependency — no actual decode capability. Stage 4 needs real PNG
  decode for the `image-path`/`image_path`/legacy `icon_data` hint aliases
  (arbitrary file paths). So: `iced = { features = [...,
  "image-without-codecs"] }` (not plain `"image"`) **plus** a direct
  `image = { version = "0.25", default-features = false, features = ["png"]
  }`. `png` resolves to `dep:png` only (verified in the crate manifest) — no
  extra transitive codec weight. The raw `image-data` hint (`iiibiiay`) is
  NOT codec-encoded (raw RGBA + rowstride) and needs no codec at all —
  Stage 4 converts it by hand.
- **Dependency versions** (all confirmed against `Cargo.lock` after a real
  `cargo build`, not guessed):
  - `iced = "0.14"` → resolves 0.14.0, `default-features = false`, features
    `tokio, svg, advanced, image-without-codecs`.
  - `image = "0.25"` → resolves 0.25.10, `default-features = false`,
    features `["png"]`.
  - `iced_layershell = "0.19"` → resolves 0.19.1.
  - `saola-theme` → git tag `saola-theme-v0.13.0`, `version = "0.13.0"`
    (verified: `git show saola-theme-v0.13.0:crates/saola-theme/Cargo.toml`
    reports `version = "0.13.0"` in the saola-theme checkout). This is
    newer than every other sibling's current pin (`saola-theme-v0.5.0`) —
    expected, this repo starts fresh after several theme releases.
  - `zbus = "5"` → resolves 5.19.0, `default-features = false`, features
    `tokio`.
  - `toml = "0.9"` → resolves 0.9.12+spec-1.1.0 (matches the family's `0.9`
    line — `saola-capture`, `saola-files`, `saola-theme`'s own
    `saola-tokens` all pin `toml = "0.9"`, so this unifies rather than
    duplicating).
  - `tracing = "0.1"` → resolves 0.1.44.
  - `tracing-subscriber = { version = "0.3", features = ["env-filter"] }` →
    resolves 0.3.23.
  - `futures = "0.3"` → resolves 0.3.34.
- **`[profile.dev.package."*"] opt-level = 3`** — carried over from
  `saola-capture`'s Cargo.toml (its own dated teaching note explains the
  4-second-screenshot debug-build regression it fixes). Applied here
  pre-emptively since the same dependency stack (iced renderer, PNG decode)
  is hot on the same kind of per-event path (every incoming `Notify` call,
  every redraw) — not yet measured in this repo specifically, since nothing
  real runs yet.
- **Version field**: `0.1.0` (not `0.1.0-dev`, which `saola-capture` uses
  but is the older convention) — matches the two newest edition-2024
  siblings, `saola-files` and `saola-session`, both `version = "0.1.0"`.
- **README Schema section** is explicitly framed as "planned, Stage 2 reads
  it" — the binary does not parse `notifications.toml` yet. Update that
  framing once Stage 2 lands.

## Gotchas for Stage 2+

- `cargo build`/`clippy`/`test` all require the system packages
  `libxkbcommon-dev libwayland-dev` (or the runtime equivalents) to link —
  matches CI's `APT_BUILD_DEPS`. They were already present on this machine;
  not verified from a clean container.
- The `saola-theme` git dependency means `cargo fetch`/`build` needs network
  + git access to `github.com/JorDunn/saola-theme` on first build (and after
  any `Cargo.lock` deletion). No vendoring is set up.
- `Cargo.lock` now exists and is checked into the working tree (not
  committed by this stage — nothing was committed, per scope). Do not
  delete it casually; regenerating it re-resolves `saola-theme`'s exact
  commit pin, which is fine (tag-pinned) but re-downloads the whole tree.
- No tests exist yet (0 tests, expected — nothing testable exists at
  Stage 1). Stage 2 is the first stage with a `test-driven-development`
  skill binding and real unit tests.
- `docs/SAOLA-STYLE-GUIDE.md` must be re-vendored (byte-identical copy, not
  hand-edited) any time the `saola-theme` tag pin bumps — do not let it
  drift from the pinned tag's own copy.

## Verify command result (paste)

```
$ cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build
...
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.39s
     Running unittests src/main.rs (target/debug/deps/saola_notifications-...)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.27s
```

All four steps exit 0. Clippy ran with zero warnings across the full
dependency tree (iced 0.14.0, iced_layershell 0.19.1, saola-theme 0.13.0,
zbus 5.19.0, image 0.25.10, and their transitive graph).
