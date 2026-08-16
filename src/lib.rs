//! `kagen` — 加減, "adjustment by degrees; the right amount".
//!
//! The half of a homeostasis loop that selects a **named quality rung**: a
//! two-sided dead-band classifier ([`Zone`]), an ordered [`Ladder`] over a
//! derive-emitted variant registry, and a [`Governor`] whose hysteresis makes
//! tier oscillation structurally hard.
//!
//! ```
//! use kagen::{Governor, Band};
//!
//! #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//! enum Quality { Off, Low, Medium, High }
//! // stands in for `#[derive(AllVariants)]`
//! const ALL: &[Quality] = &[Quality::Off, Quality::Low, Quality::Medium, Quality::High];
//!
//! let mut g = Governor::new(ALL, Quality::High)?;
//! // per frame: 15ms spent against a 16.6ms budget
//! let step = g.tick_frame(15_000, 16_666);
//! if step.changed() { /* push the new rung at the renderer */ }
//! # Ok::<(), kagen::LadderError>(())
//! ```
//!
//! # Why this is a SIBLING of `breathe_control::ControlLaw`, not an impl of it
//!
//! This crate exists because the same classifier was written twice in the
//! fleet — once in `breathe-control` over bytes, once in `mado` over
//! microseconds — with a byte-identical upper threshold and nobody noticing.
//! The obvious conclusion is that the governor should simply *be* a
//! `ControlLaw`. It should not, and the reasons are worth stating here because
//! they will otherwise be rediscovered:
//!
//! **1. The actuator is on the other side of the ratio.** `breathe` moves the
//! *denominator*: utilization is high, so grow the capacity. A tier governor
//! moves the *numerator*: utilization is high, so shed the demand. The
//! arithmetic survives that flip, which is exactly what makes it a trap — the
//! safety proof does not. `safety_clamp`'s guarantee is "never carve below what
//! the workload has demonstrated it needs", and under the flip a "shrink"
//! becomes *giving budget back* while the never-carve-below floor would be
//! computed against a frame budget that is exogenous — a refresh rate is not
//! ours to floor. Every `Decision` variant reads backwards. You would inherit
//! the vocabulary and discard the proof, which is worse than having neither.
//!
//! **2. The codomain is a closed named ladder, not a scalar.** `Proposal::
//! Target(u64)` is a continuous magnitude in base units; a rung is a position
//! on a finite, ordered, *named* set whose members have no additive unit. The
//! cost of one rung is exactly what a closed loop must *measure*, so encoding
//! it as a number at authoring time assumes away the problem.
//!
//! **3. The hysteresis has nowhere to live.** `ControlLaw::propose` takes
//! `&self` and a single sample — memoryless by construction, which is what lets
//! one safety gate be proven for every law at once. This governor's entire
//! guarantee is inter-tick state (two streak counters). There is no parameter
//! to put it in, and adding one would weaken the property that makes the law
//! set provable.
//!
//! So the two share their **classifier** and nothing below it. [`zone`] is that
//! shared function, and the edge is deliberately pointed so that
//! `BandLaw::propose` can one day adopt it and delete its copy — a shared
//! function owned by one of its two callers is how the fleet got two copies in
//! the first place.
//!
//! # Honest tiers
//!
//! Stated in `selo`'s vocabulary, and not rounded up:
//!
//! | invariant | tier |
//! |---|---|
//! | a duplicate rung reaches a governor | **parse-time-rejected** — [`Ladder::parse`] refuses it |
//! | a ceiling that is not a rung reaches a governor | **parse-time-rejected** — [`Governor::new`] refuses it |
//! | the governor leaves the ladder | **truly-unrep** — every move is a [`Ladder`] index step, saturating at both ends |
//! | "nothing left to give" reported as "nothing to do" | **only-mitigated (C1)** — [`Step::AtFloor`] is a distinct variant, but nothing forces a caller to match it |
//! | the ladder is declared out of cost order | **only-mitigated (C1)** — no type proves `Low` is cheaper than `Medium`. Falsifiable at runtime though: if stepping down does not reduce measured cost, the declaration is a lie, so a closed loop can audit its own ladder |
//! | the tier oscillates | **only-mitigated (C1)** — three composed mechanisms, proven against a 1000-tick alternating signal. "No oscillation for any input" quantifies over an unbounded sequence |
//!
//! The measurement feeding [`zone`] carries its own ceiling and it belongs to
//! the caller, not to this crate: a CPU-side frame time cannot observe a
//! GPU-bound stall, so a consumer measuring that way is `only-mitigated (C2)`
//! on the budget axis no matter how good the hysteresis above it is. A consumer
//! that owns a presentation clock can do better. Neither is this crate's claim
//! to make.

#![forbid(unsafe_code)]

mod governor;
mod ladder;
mod zone;

pub use governor::{DOWN_AFTER, Governor, Step, UP_AFTER};
pub use ladder::{Ladder, LadderError};
pub use zone::{Band, Zone, budget_us_for_fps, frame_zone, zone};

#[cfg(feature = "zenmai")]
pub use governor::GovernorMachine;
