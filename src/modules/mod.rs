//! Surface modules.
//!
//! One file per module, each exposing a state struct, its own `pub enum
//! Message`, a `view` that takes `&Theme`, and a `subscription` — the
//! pattern `saola-panel::modules::mod` documents in full and AGENTS.md's
//! "Module pattern" section makes binding here. The daemon's outer
//! `Message` (`main.rs`) nests each module's enum as one variant
//! (`Message::Toast(toast::Message)`) and composes the module's view and
//! subscription with `.map` at the point they join the daemon
//! (`Element::map`, `Subscription::map`).
//!
//! # Two documented deviations from the panel's shape
//!
//! Both come from the same fact: this crate's modules are **surfaces over
//! one shared model**, where a panel module is a self-contained readout of
//! its own private source.
//!
//! 1. **`view` takes more than `&Theme`.** The notification model lives in
//!    [`crate::store::Store`], which the toast surface and (Stage 7) the
//!    notification centre both render — so it cannot be owned by either
//!    module. A module's `view` therefore reads
//!    `view(&self, theme: &Theme, store: &Store, now: Instant)`: the theme
//!    first, exactly as the pattern says, then the model and the clock
//!    injected alongside it. The clock is a parameter for the same reason
//!    it is one throughout `store.rs` — `Instant::now()` is read in
//!    `main.rs` and nowhere else, which is what keeps the animation and
//!    expiry math unit-testable (AGENTS.md's Resilience rules).
//!
//! 2. **A module's state struct holds only what the store does not.**
//!    [`toast::Toasts`] owns no notifications at all — the stack, the
//!    stopwatches and the replace policy are all `Store`'s. What it does
//!    own is genuinely view-level: which card the pointer is inside. A
//!    module here is thin by design; when a behaviour is about *what a
//!    notification is*, it belongs in `store.rs` where it can be tested
//!    without a compositor.
//!
//! Everything else the panel's doc comment says still applies verbatim —
//! zero hardcoded colors/sizes, three-color rule, an absent source renders
//! nothing rather than killing the process, and every module maps to a
//! signal rather than a poll (the toast stack's animation tick is
//! AGENTS.md's one documented exception, and it is gated to run only while
//! a card is actually on screen — see [`toast::Toasts::subscription`]).

pub mod toast;
