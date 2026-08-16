//! An ordered quality ladder over a caller-supplied variant registry.
//!
//! ## Why this takes a registry by value instead of bounding on a trait
//!
//! The obvious move is `T: shikumi::ClosedAxis`, which is the fleet's real
//! trait for an enumerable closed axis. It is the wrong move *here*, and the
//! reason is cost rather than taste: `ClosedAxis` lives in `shikumi`, which
//! carries figment, serde and a large combinatorial cube. A governor is ticked
//! inside a render loop — on a 120 Hz compositor with several knobs, thousands
//! of times a second — and pulling a configuration framework into that crate to
//! obtain one associated constant is a poor trade.
//!
//! So [`Ladder::parse`] takes `&'static [T]` directly, which is exactly what
//! `pleme-allvariants-derive` emits:
//!
//! ```ignore
//! #[derive(Clone, Copy, PartialEq, Eq, Debug, AllVariants)]
//! enum Quality { Off, Low, Medium, High }
//!
//! let ladder = Ladder::parse(Quality::ALL)?;
//! ```
//!
//! The derive is used exactly as designed — it emits an *inherent* `const ALL`
//! precisely so it can be handed around — and this crate stays
//! dependency-free. A consumer that already implements `ClosedAxis` loses
//! nothing: `T::ALL` is still just a slice.
//!
//! ## Declaration order IS the ladder
//!
//! Index `0` is the floor (cheapest, most degraded) and the last index is the
//! top (most expensive, best). This is a convention the type cannot check —
//! see [`LadderError`] for what it *can* — and it is graded honestly in the
//! crate's ledger as `only-mitigated (C1)`.
//!
//! It is not, however, unfalsifiable. A closed loop that measures what each
//! rung actually costs can *detect* a mis-ordered ladder: if stepping from
//! `Medium` to `Low` does not reduce the measured cost, the declaration is a
//! lie. A governor can audit its own ladder; it simply cannot do so at compile
//! time.

/// Why a [`Ladder`] could not be built from a registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderError {
    /// Fewer than two rungs. A one-rung ladder has no step to take, so a
    /// governor over it is a governor that can never act — almost certainly a
    /// mistake at the call site rather than an intended configuration.
    TooShort,
    /// The registry contains the same rung twice, at the given indices.
    ///
    /// A duplicate makes [`Ladder::rung`] ambiguous and would let `down` then
    /// `up` land somewhere other than where it started, which reads downstream
    /// as an oscillation bug with no oscillating input.
    Duplicate {
        /// The earlier index.
        first: usize,
        /// The later index holding an equal value.
        second: usize,
    },
    /// A ceiling was supplied that is not a member of the registry.
    CeilingNotOnLadder,
}

impl core::fmt::Display for LadderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooShort => f.write_str("a ladder needs at least two rungs to have a step"),
            Self::Duplicate { first, second } => {
                f.write_str("duplicate rung at indices ")?;
                core::fmt::Display::fmt(first, f)?;
                f.write_str(" and ")?;
                core::fmt::Display::fmt(second, f)
            }
            Self::CeilingNotOnLadder => f.write_str("the ceiling is not a rung on this ladder"),
        }
    }
}

impl core::error::Error for LadderError {}

/// An ordered ladder of quality rungs, cheapest first.
///
/// Cheap to copy: it holds a `&'static` slice and nothing else, so passing it
/// through a per-frame state struct costs a pointer and a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder<T: 'static> {
    rungs: &'static [T],
}

impl<T: Copy + Eq + 'static> Ladder<T> {
    /// Build a ladder from a variant registry, cheapest rung first.
    ///
    /// # Errors
    /// [`LadderError::TooShort`] if fewer than two rungs;
    /// [`LadderError::Duplicate`] if any rung appears twice.
    pub fn parse(rungs: &'static [T]) -> Result<Self, LadderError> {
        if rungs.len() < 2 {
            return Err(LadderError::TooShort);
        }
        // O(n^2), deliberately: ladders are a handful of rungs, and this keeps
        // the crate free of a hash dependency and of a `Hash`/`Ord` bound that
        // would narrow what a consumer's tier enum has to derive.
        for (i, a) in rungs.iter().enumerate() {
            for (j, b) in rungs.iter().enumerate().skip(i + 1) {
                if a == b {
                    return Err(LadderError::Duplicate { first: i, second: j });
                }
            }
        }
        Ok(Self { rungs })
    }

    /// Every rung, cheapest first.
    #[must_use]
    pub fn rungs(&self) -> &'static [T] {
        self.rungs
    }

    /// How many rungs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rungs.len()
    }

    /// Always `false` — [`parse`](Self::parse) rejects a ladder shorter than
    /// two rungs, so an empty one cannot be constructed. Present because
    /// clippy asks for it beside [`len`](Self::len), and it documents that the
    /// emptiness question is already settled at the boundary.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The index of `t`, or `None` if it is not on this ladder.
    #[must_use]
    pub fn rung(&self, t: T) -> Option<usize> {
        self.rungs.iter().position(|r| *r == t)
    }

    /// `true` if `t` is a rung on this ladder.
    #[must_use]
    pub fn contains(&self, t: T) -> bool {
        self.rung(t).is_some()
    }

    /// The cheapest rung.
    #[must_use]
    pub fn floor(&self) -> T {
        self.rungs[0]
    }

    /// The most expensive rung.
    #[must_use]
    pub fn top(&self) -> T {
        self.rungs[self.rungs.len() - 1]
    }

    /// One rung cheaper, saturating at the floor.
    ///
    /// A `t` that is not on this ladder returns the floor: the caller is
    /// already in a state this ladder cannot describe, and shedding to the
    /// cheapest rung is the safe direction. It is never a panic.
    #[must_use]
    pub fn down(&self, t: T) -> T {
        match self.rung(t) {
            Some(0) | None => self.floor(),
            Some(i) => self.rungs[i - 1],
        }
    }

    /// One rung richer, saturating at `ceiling`.
    ///
    /// The ceiling is the user's declared maximum, so climbing never passes it
    /// even when the ladder has further rungs. An unknown `t` or an unknown
    /// `ceiling` yields the floor rather than a panic — the conservative
    /// direction, since the alternative is to grant quality on the strength of
    /// a value we could not place.
    #[must_use]
    pub fn up(&self, t: T, ceiling: T) -> T {
        let (Some(i), Some(cap)) = (self.rung(t), self.rung(ceiling)) else {
            return self.floor();
        };
        if i >= cap { self.rungs[cap] } else { self.rungs[i + 1] }
    }

    /// Check that `ceiling` is on this ladder.
    ///
    /// # Errors
    /// [`LadderError::CeilingNotOnLadder`] if it is not.
    pub fn validate_ceiling(&self, ceiling: T) -> Result<(), LadderError> {
        if self.contains(ceiling) { Ok(()) } else { Err(LadderError::CeilingNotOnLadder) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Q {
        Off,
        Low,
        Medium,
        High,
    }
    impl Q {
        // Stands in for what `pleme-allvariants-derive` emits.
        const ALL: &'static [Q] = &[Q::Off, Q::Low, Q::Medium, Q::High];
    }

    fn ladder() -> Ladder<Q> {
        Ladder::parse(Q::ALL).expect("the fixture ladder is well-formed")
    }

    #[test]
    fn a_registry_becomes_an_ordered_ladder() {
        let l = ladder();
        assert_eq!(l.len(), 4);
        assert_eq!(l.floor(), Q::Off);
        assert_eq!(l.top(), Q::High);
        assert_eq!(l.rung(Q::Medium), Some(2));
    }

    #[test]
    fn a_one_rung_registry_is_refused() {
        // A governor over it could never act; better a typed error at
        // construction than a silently inert controller.
        const ONE: &[Q] = &[Q::High];
        assert_eq!(Ladder::parse(ONE), Err(LadderError::TooShort));
        const NONE: &[Q] = &[];
        assert_eq!(Ladder::parse(NONE), Err(LadderError::TooShort));
    }

    #[test]
    fn a_duplicate_rung_is_refused_and_names_both_indices() {
        const DUP: &[Q] = &[Q::Off, Q::Low, Q::Off];
        assert_eq!(Ladder::parse(DUP), Err(LadderError::Duplicate { first: 0, second: 2 }));
    }

    #[test]
    fn stepping_down_saturates_at_the_floor() {
        let l = ladder();
        assert_eq!(l.down(Q::High), Q::Medium);
        assert_eq!(l.down(Q::Low), Q::Off);
        assert_eq!(l.down(Q::Off), Q::Off, "the floor is a fixed point");
    }

    #[test]
    fn stepping_up_saturates_at_the_declared_ceiling_not_the_top() {
        let l = ladder();
        assert_eq!(l.up(Q::Off, Q::High), Q::Low);
        assert_eq!(l.up(Q::Low, Q::Medium), Q::Medium);
        assert_eq!(
            l.up(Q::Medium, Q::Medium),
            Q::Medium,
            "the ceiling is a fixed point — a user's declared maximum is not a \
             suggestion the governor may exceed once it feels calm"
        );
    }

    #[test]
    fn a_state_above_the_ceiling_is_pulled_down_to_it() {
        // Reachable for real: a user lowers the ceiling while the governor is
        // parked on a richer rung. Climbing must not leave it stranded above.
        let l = ladder();
        assert_eq!(l.up(Q::High, Q::Low), Q::Low);
    }

    #[test]
    fn an_unknown_rung_yields_the_floor_rather_than_a_panic() {
        const PARTIAL: &[Q] = &[Q::Off, Q::Low];
        let l = Ladder::parse(PARTIAL).unwrap();
        assert_eq!(l.down(Q::High), Q::Off);
        assert_eq!(l.up(Q::High, Q::Low), Q::Off);
        assert_eq!(l.up(Q::Off, Q::High), Q::Off, "an unplaceable ceiling never grants quality");
    }

    #[test]
    fn down_then_up_returns_to_the_start_away_from_the_bounds() {
        // The property a duplicate rung would break, which is why parse refuses
        // one. Checked across every interior rung rather than a single sample.
        let l = ladder();
        for &q in &Q::ALL[1..] {
            assert_eq!(l.up(l.down(q), Q::High), q, "down-then-up must be identity for {q:?}");
        }
    }

    #[test]
    fn a_ceiling_off_the_ladder_is_reported() {
        const PARTIAL: &[Q] = &[Q::Off, Q::Low];
        let l = Ladder::parse(PARTIAL).unwrap();
        assert_eq!(l.validate_ceiling(Q::High), Err(LadderError::CeilingNotOnLadder));
        assert_eq!(l.validate_ceiling(Q::Low), Ok(()));
    }

    #[test]
    fn errors_render_without_format_machinery() {
        // Typed emission: Display is the render surface.
        assert!(LadderError::TooShort.to_string().contains("two rungs"));
        assert!(LadderError::Duplicate { first: 0, second: 2 }.to_string().contains("0 and 2"));
    }
}
