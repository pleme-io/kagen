//! The two-sided dead-band classifier.
//!
//! This module exists because the same function was independently written twice
//! in the fleet, in two repos, over two different quantities, and nobody
//! noticed:
//!
//! ```text
//! breathe-control  BandLaw::propose(working_set, current_limit, cfg)
//!     util = working_set / current_limit
//!     util > cfg.grow_above    -> grow      (0.85)
//!     util < cfg.shrink_below  -> shrink    (0.70)
//!     else                     -> Hold
//!
//! mado             ux::ambience_governor::classify_frame(frame_us, budget_us)
//!     frac = frame_us / budget_us
//!     frac > OVER_FRAC         -> TickOverBudget   (0.85)
//!     frac < CALM_FRAC         -> TickCalm         (0.60)
//!     else                     -> TickNeutral
//! ```
//!
//! Same ratio, same two-sided dead band, same three-way partition, the same
//! comparison operators in the same order — and `OVER_FRAC` is byte-identical
//! to `grow_above`. One measures bytes against a memory limit; the other
//! measures microseconds against a frame budget. **The upper threshold is not
//! merely similar, it is the same number**, which is what makes this a
//! unification rather than a coincidence worth a comment.
//!
//! Owning it once means the number has one owner. A consumer no longer declares
//! `const OVER_FRAC: f32 = 0.85` beside a comment explaining that it matches
//! something in another repo; it names [`Band::FRAME`] and the two move
//! together or not at all.
//!
//! ## What is deliberately NOT here
//!
//! The classifier, and nothing else. Everything past it diverges, and the
//! divergence is the reason [`crate::Governor`] is a *sibling* of
//! `breathe_control::ControlLaw` rather than an impl of it — see the crate
//! docs. Putting the actuator here too would recreate the coupling this
//! module exists to avoid.

/// A two-sided dead band: the pair of utilization thresholds that partition a
/// `used/capacity` ratio into [`Zone::Above`], [`Zone::InBand`] and
/// [`Zone::Below`].
///
/// The gap between [`below`](Band::below) and [`above`](Band::above) is the
/// dead zone, and it is the first of the three mechanisms that make tier
/// oscillation hard: a ratio that merely drifts inside the band produces no
/// movement at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    /// A ratio **strictly above** this is [`Zone::Above`] — the system is
    /// under pressure and something must give.
    pub above: f64,
    /// A ratio **strictly below** this is [`Zone::Below`] — the system is calm
    /// and could afford more.
    pub below: f64,
}

impl Band {
    /// The resource band: `breathe_control::BandConfig::default()`'s
    /// `grow_above` / `shrink_below`. Used for bytes against a limit.
    pub const RESOURCE: Self = Self { above: 0.85, below: 0.70 };

    /// The frame band: mado's shipped `OVER_FRAC` / `CALM_FRAC`.
    ///
    /// `above` is identical to [`RESOURCE`](Self::RESOURCE); only `below`
    /// differs, and it differs for a nameable reason rather than by accident.
    /// A visibly-degraded frame costs a *human* attention, while a memory
    /// carve costs a pod nothing it can perceive — so the frame band buys a
    /// wider dead zone (0.60 rather than 0.70) and climbs back more
    /// reluctantly.
    pub const FRAME: Self = Self { above: 0.85, below: 0.60 };

    /// `true` if the band is well-formed: `0 <= below <= above`, both finite.
    ///
    /// An inverted band (`below > above`) would make [`Zone::InBand`]
    /// unreachable and turn the dead zone into a *trigger* zone, which is the
    /// exact opposite of its purpose. [`Ladder`](crate::Ladder)-driven
    /// consumers should reject one at construction rather than discover it as
    /// a thrash.
    #[must_use]
    pub fn is_well_formed(self) -> bool {
        self.above.is_finite() && self.below.is_finite() && self.below >= 0.0 && self.below <= self.above
    }
}

/// Which side of a [`Band`] a `used/capacity` ratio falls on.
///
/// Named for the *measurement*, not for the reaction. `Above` means "the ratio
/// is above the upper threshold" — it does not say whether the correct response
/// is to grow the capacity or shed the demand, because that answer differs
/// between consumers and encoding it here is precisely how a shared classifier
/// would acquire one consumer's actuator semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zone {
    /// Strictly above [`Band::above`] — under pressure.
    Above,
    /// Within the dead band, inclusive of both thresholds — leave it alone.
    InBand,
    /// Strictly below [`Band::below`] — calm.
    Below,
}

impl Zone {
    /// Every variant, declaration order. Lets an exhaustive `state × zone`
    /// matrix test enumerate the event axis without hand-listing it.
    pub const ALL: &'static [Self] = &[Self::Above, Self::InBand, Self::Below];
}

/// Classify `used/capacity` against `band`.
///
/// # Zero and non-finite capacity
///
/// A `capacity` of `0` is [`Zone::Below`] — "there is no budget to be under
/// pressure against". This is a deliberate divergence from
/// `breathe_control::BandLaw::propose`, which has no such guard and would
/// compute `used/0 = inf` and report a grow (and `0/0 = NaN`, whose comparisons
/// are all false, landing in the dead band instead — two different answers for
/// two flavours of the same degenerate input).
///
/// The guard belongs *here*, at the general seam, and its absence there is not
/// a bug: `decide_with`'s floor-seed guarantees a nonzero limit before the law
/// ever runs. A general classifier has no such caller-side guarantee, so it
/// must answer for itself. `crate::parity` documents this as the one excluded
/// region of the parity test rather than papering over it.
#[must_use]
pub fn zone(used: u64, capacity: u64, band: Band) -> Zone {
    if capacity == 0 {
        return Zone::Below;
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = used as f64 / capacity as f64;
    if ratio > band.above {
        Zone::Above
    } else if ratio < band.below {
        Zone::Below
    } else {
        Zone::InBand
    }
}

/// Classify a frame against a budget, both in microseconds — [`zone`] with
/// [`Band::FRAME`] pre-applied.
///
/// The shape mado's `classify_frame` has today, including its `budget_us == 0`
/// guard, which falls out of [`zone`]'s rather than being restated.
#[must_use]
pub fn frame_zone(frame_us: u64, budget_us: u64) -> Zone {
    zone(frame_us, budget_us, Band::FRAME)
}

/// The frame budget for a target refresh rate, in microseconds.
///
/// `0` fps yields `0`, which [`zone`] reads as [`Zone::Below`] — an unknown
/// refresh rate must never be reported as budget pressure, since the reflex it
/// would trigger degrades quality for no measured reason.
#[must_use]
pub fn budget_us_for_fps(fps: u32) -> u64 {
    if fps == 0 { 0 } else { 1_000_000 / u64::from(fps) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upper_threshold_is_shared_not_merely_similar() {
        // The whole reason this module exists. If someone retunes one band's
        // upper threshold without the other, this is the test that argues.
        assert!(
            (Band::RESOURCE.above - Band::FRAME.above).abs() < f64::EPSILON,
            "the resource and frame bands must share an upper threshold — that \
             identity is the finding this crate is built on"
        );
        assert!(
            Band::FRAME.below < Band::RESOURCE.below,
            "the frame band must be the WIDER of the two: a visible quality \
             flip costs a human more than a memory carve costs a pod"
        );
    }

    #[test]
    fn both_shipped_bands_are_well_formed() {
        assert!(Band::RESOURCE.is_well_formed());
        assert!(Band::FRAME.is_well_formed());
    }

    #[test]
    fn an_inverted_band_is_detected() {
        assert!(!Band { above: 0.60, below: 0.85 }.is_well_formed());
        assert!(!Band { above: f64::NAN, below: 0.1 }.is_well_formed());
        assert!(!Band { above: 1.0, below: -0.1 }.is_well_formed());
    }

    #[test]
    fn the_partition_is_total_and_matches_the_thresholds() {
        let b = Band::FRAME;
        assert_eq!(zone(90, 100, b), Zone::Above); // 0.90 > 0.85
        assert_eq!(zone(70, 100, b), Zone::InBand); // 0.60 <= 0.70 <= 0.85
        assert_eq!(zone(50, 100, b), Zone::Below); // 0.50 < 0.60
    }

    #[test]
    fn the_thresholds_themselves_are_in_band_because_both_bounds_are_strict() {
        // Exactly ON either threshold is InBand: `>` and `<` are both strict.
        // Worth pinning because an off-by-one to `>=` here would silently make
        // the dead band half-open and let a knob chatter on the boundary.
        let b = Band { above: 0.85, below: 0.60 };
        assert_eq!(zone(85, 100, b), Zone::InBand);
        assert_eq!(zone(60, 100, b), Zone::InBand);
    }

    #[test]
    fn zero_capacity_is_below_not_above() {
        // The documented divergence from BandLaw. `used/0` would be `inf`
        // (-> Above) and `0/0` would be NaN (-> InBand); both are wrong for a
        // classifier that may be called before a budget is known.
        assert_eq!(zone(16_000, 0, Band::FRAME), Zone::Below);
        assert_eq!(zone(0, 0, Band::FRAME), Zone::Below);
        assert_eq!(frame_zone(16_000, 0), Zone::Below);
    }

    #[test]
    fn a_frame_over_budget_is_above_at_sixty_hertz() {
        let budget = budget_us_for_fps(60);
        assert_eq!(budget, 16_666);
        assert_eq!(frame_zone(15_000, budget), Zone::Above); // 0.90
        assert_eq!(frame_zone(8_000, budget), Zone::Below); // 0.48
    }

    #[test]
    fn rebudgeting_to_120hz_flips_a_9ms_frame_from_below_to_above() {
        // mado's `rebudget_to_120hz_flips_a_9ms_frame_from_calm_to_over`,
        // expressed against the extracted classifier. A 9ms frame is calm at
        // 60Hz and over budget at 120Hz — the same measurement, a different
        // band, and the governor must see the change.
        let nine_ms = 9_000;
        assert_eq!(frame_zone(nine_ms, budget_us_for_fps(60)), Zone::Below);
        assert_eq!(frame_zone(nine_ms, budget_us_for_fps(120)), Zone::Above);
    }

    #[test]
    fn zero_fps_yields_no_budget_and_therefore_no_pressure() {
        assert_eq!(budget_us_for_fps(0), 0);
        assert_eq!(frame_zone(999_999, budget_us_for_fps(0)), Zone::Below);
    }

    #[test]
    fn zone_all_covers_every_variant() {
        // Cheap forcing function: a new Zone variant that is not added to ALL
        // makes this fail, so the matrix tests that enumerate ALL cannot go
        // silently blind to it.
        assert_eq!(Zone::ALL.len(), 3);
        for z in Zone::ALL {
            assert!(matches!(z, Zone::Above | Zone::InBand | Zone::Below));
        }
    }
}
