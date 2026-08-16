//! The tier governor: hysteresis over a [`Ladder`], driven by [`Zone`].
//!
//! Generalized from `mado`'s shipped `ux::ambience_governor`, which has run in
//! a live render loop against one knob (aurora quality) with six tests
//! including a 1000-tick no-oscillation proof. Everything here that looks like
//! a design decision is a decision that was already made and already survived
//! contact with a real workload; this module's contribution is to make it
//! reusable and to name the parts.
//!
//! ## The three anti-oscillation mechanisms
//!
//! A naive governor — "over budget, step down; under budget, step up" — is a
//! flicker generator: one step changes the cost, which changes the
//! classification, which steps back. Three mechanisms compose to prevent it,
//! and all three are load-bearing:
//!
//! 1. **The dead band.** [`Zone::InBand`] is a third answer between the two
//!    triggers, so a ratio that merely drifts produces no movement. Lives in
//!    [`crate::zone`].
//! 2. **Asymmetric streaks.** Shedding is fast ([`DOWN_AFTER`], ~0.5 s at
//!    60 Hz); recovering is slow ([`UP_AFTER`], ~5 s). The 10x asymmetry means
//!    a governor that has just shed quality will not try to reclaim it until
//!    the calm has clearly persisted, so a marginal workload settles instead of
//!    hunting.
//! 3. **The resets.** Each zone clears the *opposing* streak (so an
//!    alternating signal can never accumulate toward either edge), and
//!    [`Zone::InBand`] clears **both**. These are the subtlest of the three
//!    and the ones most likely to be dropped by a reimplementation.
//!
//! Each mechanism has its own guard, and each guard was verified by deleting
//! the mechanism and confirming the guard goes red — because a test that has
//! never failed is not evidence, it is decoration. Two findings from doing
//! that, both recorded here so they are not rediscovered:
//!
//! - `no_oscillation_on_mixed_or_noisy_observations` must start the governor
//!   **mid-ladder**. Resting at the ceiling it can only move down, so an
//!   erroneously accumulating calm streak climbs into [`Step::AtCeiling`] and
//!   the tier never moves — the test passes for the wrong reason and is blind
//!   to upward drift. Measured: with the resets deleted, the ceiling-resting
//!   version stayed green.
//! - That test proves the **mutual** reset, not the neutral one. An
//!   alternating signal never emits [`Zone::InBand`], so it cannot exercise
//!   it. They are separate tests for that reason.
//! - Mechanism 2 (the asymmetry) was **unguarded** until
//!   `the_shed_fast_reclaim_slow_asymmetry_stops_a_marginal_workload_hunting`
//!   was added: collapsing [`UP_AFTER`] to [`DOWN_AFTER`] passed every other
//!   test in the file. Neither no-oscillation test can see it, because
//!   alternating and neutral signals never build a long enough one-sided
//!   streak for the asymmetry to matter.
//!
//! ## Scope: this is the SOLO governor
//!
//! One governor over one knob, which is what mado ships and what a first
//! consumer needs. Coordinating several governors that draw on one budget is a
//! genuinely different problem with its own failure mode — N governors watching
//! one signal accumulate streaks in lockstep and all step down on the same
//! tick, a cliff rather than a step, while each one individually still passes
//! the no-oscillation test. That coordination layer is deliberately not here
//! yet; [`Step`] is `#[non_exhaustive]` so it can gain the variant it will need
//! without a breaking change.

use crate::{Ladder, LadderError, Zone};

/// Consecutive [`Zone::Above`] observations before a step down — ~0.5 s at
/// 60 Hz. Shedding is deliberately fast: the cost of holding a blown budget is
/// paid every frame.
pub const DOWN_AFTER: u32 = 30;

/// Consecutive [`Zone::Below`] observations before a step up — ~5 s at 60 Hz.
///
/// `UP_AFTER >> DOWN_AFTER` is half the no-oscillation guarantee (the dead band
/// is the other half). Recovering slowly is what stops a marginal workload from
/// hunting between two rungs.
pub const UP_AFTER: u32 = 300;

/// The outcome of one [`Governor::advance`].
///
/// `#[non_exhaustive]`: a coordination layer will need to report "the streak
/// matured but another knob was given the step this tick", and that must be
/// addable without breaking consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step<T> {
    /// No movement. The streak is still building, or the zone was neutral.
    Hold {
        /// The unchanged rung.
        state: T,
    },
    /// The rung changed.
    Stepped {
        /// The rung before.
        from: T,
        /// The rung after.
        to: T,
    },
    /// A step down was earned, but the governor is already on the cheapest
    /// rung — **there is nothing left to give**.
    ///
    /// Deliberately not [`Step::Hold`]. "Nothing to do" and "nothing left to
    /// do" are opposite claims: the first says the system is fine, the second
    /// says the reflex is spent and the pressure is unresolved. Collapsing them
    /// is how a saturated controller reads as a converged one — the same
    /// distinction `breathe_control::Decision::ReclaimWithheld` exists to make.
    AtFloor {
        /// The floor rung the governor is stuck on.
        state: T,
    },
    /// A step up was earned, but the governor is already at the declared
    /// ceiling. The system is calm and the user's maximum is honoured.
    AtCeiling {
        /// The ceiling rung.
        state: T,
    },
}

impl<T: Copy> Step<T> {
    /// The rung the governor rests on after this step.
    pub fn state(self) -> T {
        match self {
            Step::Hold { state } | Step::AtFloor { state } | Step::AtCeiling { state } => state,
            Step::Stepped { to, .. } => to,
        }
    }

    /// `true` if the rung actually changed — the predicate a caller uses to
    /// decide whether to push a new value at the renderer.
    pub fn changed(self) -> bool {
        matches!(self, Step::Stepped { .. })
    }
}

/// A hysteresis governor over an ordered [`Ladder`].
///
/// `Copy` and allocation-free: it is a handful of integers plus a `&'static`
/// slice, so per-frame use costs nothing and it can live directly in a
/// renderer struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Governor<T: 'static> {
    ladder: Ladder<T>,
    state: T,
    ceiling: T,
    over_streak: u32,
    calm_streak: u32,
    down_after: u32,
    up_after: u32,
}

impl<T: Copy + Eq + 'static> Governor<T> {
    /// Build a governor over `rungs`, starting at `ceiling` and never climbing
    /// above it.
    ///
    /// Starting *at* the ceiling is the right default: a governor should begin
    /// by giving the user what they asked for and shed only once it has
    /// measured a reason to.
    ///
    /// # Errors
    /// [`LadderError`] if the registry is not a well-formed ladder, or if
    /// `ceiling` is not one of its rungs.
    pub fn new(rungs: &'static [T], ceiling: T) -> Result<Self, LadderError> {
        let ladder = Ladder::parse(rungs)?;
        ladder.validate_ceiling(ceiling)?;
        Ok(Self {
            ladder,
            state: ceiling,
            ceiling,
            over_streak: 0,
            calm_streak: 0,
            down_after: DOWN_AFTER,
            up_after: UP_AFTER,
        })
    }

    /// Override the streak thresholds.
    ///
    /// `down_after` should stay well below `up_after` — the asymmetry is one of
    /// the three anti-oscillation mechanisms, and inverting it will produce a
    /// governor that sheds reluctantly and reclaims eagerly, which hunts. A `0`
    /// is clamped to `1`, since a threshold of zero would step on every tick
    /// and remove the streak mechanism altogether.
    #[must_use]
    pub fn with_streaks(mut self, down_after: u32, up_after: u32) -> Self {
        self.down_after = down_after.max(1);
        self.up_after = up_after.max(1);
        self
    }

    /// The current rung.
    pub fn quality(&self) -> T {
        self.state
    }

    /// The declared ceiling.
    pub fn ceiling(&self) -> T {
        self.ceiling
    }

    /// The ladder this governor moves on.
    pub fn ladder(&self) -> Ladder<T> {
        self.ladder
    }

    /// Lower or raise the user's declared maximum.
    ///
    /// If the new ceiling is below the current rung, the governor drops to it
    /// immediately rather than waiting for a calm streak: a ceiling is a bound,
    /// and continuing to render above one the user just lowered would be
    /// ignoring an instruction.
    ///
    /// # Errors
    /// [`LadderError::CeilingNotOnLadder`] if `ceiling` is not a rung.
    pub fn set_ceiling(&mut self, ceiling: T) -> Result<(), LadderError> {
        self.ladder.validate_ceiling(ceiling)?;
        self.ceiling = ceiling;
        let (Some(cur), Some(cap)) = (self.ladder.rung(self.state), self.ladder.rung(ceiling))
        else {
            return Ok(());
        };
        if cur > cap {
            self.state = ceiling;
        }
        Ok(())
    }

    /// Feed one classified observation and get the resulting step.
    ///
    /// Total over `(state, zone)`: every pair is defined, and a pair that earns
    /// no movement is inert rather than a panic.
    pub fn advance(&mut self, zone: Zone) -> Step<T> {
        match zone {
            Zone::Above => {
                self.calm_streak = 0;
                self.over_streak = self.over_streak.saturating_add(1);
                if self.over_streak < self.down_after {
                    return Step::Hold { state: self.state };
                }
                self.over_streak = 0;
                let next = self.ladder.down(self.state);
                if next == self.state {
                    Step::AtFloor { state: self.state }
                } else {
                    let from = self.state;
                    self.state = next;
                    Step::Stepped { from, to: next }
                }
            }
            Zone::Below => {
                self.over_streak = 0;
                self.calm_streak = self.calm_streak.saturating_add(1);
                if self.calm_streak < self.up_after {
                    return Step::Hold { state: self.state };
                }
                self.calm_streak = 0;
                let next = self.ladder.up(self.state, self.ceiling);
                if next == self.state {
                    Step::AtCeiling { state: self.state }
                } else {
                    let from = self.state;
                    self.state = next;
                    Step::Stepped { from, to: next }
                }
            }
            // Mechanism 3: a neutral observation resets BOTH streaks, so a
            // mixed signal parks the tier rather than walking it.
            Zone::InBand => {
                self.over_streak = 0;
                self.calm_streak = 0;
                Step::Hold { state: self.state }
            }
        }
    }

    /// Classify `used/capacity` against `band` and advance in one call.
    pub fn tick(&mut self, used: u64, capacity: u64, band: crate::Band) -> Step<T> {
        self.advance(crate::zone(used, capacity, band))
    }

    /// Classify a frame against a budget (both microseconds) and advance —
    /// [`tick`](Self::tick) with [`crate::Band::FRAME`] applied.
    pub fn tick_frame(&mut self, frame_us: u64, budget_us: u64) -> Step<T> {
        self.advance(crate::frame_zone(frame_us, budget_us))
    }

    /// Both streak counters, `(over, calm)`. Diagnostics and tests; a consumer
    /// should drive off [`Step`].
    pub fn streaks(&self) -> (u32, u32) {
        (self.over_streak, self.calm_streak)
    }
}

/// `zenmai::Machine` marker for [`Governor`], so the governor participates in
/// the fleet's shared reducer vocabulary (`Stateful`, `Driver`,
/// `assert_total_inert`) rather than being a bespoke FSM.
///
/// Off the default path on purpose: `Machine::step` returns `Vec<Effect>`, and
/// while an empty `Vec` does not allocate, a non-empty one does. The direct
/// [`Governor::advance`] is the allocation-free call for a render loop; this
/// impl is for consumers already driving a `zenmai` machine.
#[cfg(feature = "zenmai")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernorMachine<T: 'static>(core::marker::PhantomData<T>);

#[cfg(feature = "zenmai")]
impl<T: Copy + Eq + 'static> zenmai::Machine for GovernorMachine<T> {
    type State = Governor<T>;
    type Event = Zone;
    type Effect = Step<T>;

    fn step(state: &Self::State, event: Self::Event) -> (Self::State, Vec<Self::Effect>) {
        let mut next = *state;
        let outcome = next.advance(event);
        // A `Hold` is not an effect: nothing downstream must act on it. Only a
        // step, or a saturation the caller needs to know about, is emitted —
        // which is also what makes the governor provably inert on a neutral
        // tick with both streaks already at rest.
        match outcome {
            Step::Hold { .. } => (next, Vec::new()),
            other => (next, vec![other]),
        }
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
        const ALL: &'static [Q] = &[Q::Off, Q::Low, Q::Medium, Q::High];
    }

    fn gov() -> Governor<Q> {
        Governor::new(Q::ALL, Q::High).expect("fixture ladder is well-formed")
    }

    fn feed(g: &mut Governor<Q>, z: Zone, n: u32) -> Step<Q> {
        let mut last = Step::Hold { state: g.quality() };
        for _ in 0..n {
            last = g.advance(z);
        }
        last
    }

    #[test]
    fn a_governor_starts_at_its_ceiling() {
        let g = gov();
        assert_eq!(g.quality(), Q::High);
        assert_eq!(g.ceiling(), Q::High);
    }

    #[test]
    fn a_ceiling_off_the_ladder_is_refused_at_construction() {
        // Refused at the boundary rather than saturating silently: a ceiling
        // the ladder cannot place is a call-site mistake, and a governor that
        // quietly picked a different maximum would hide it.
        const PARTIAL: &[Q] = &[Q::Off, Q::Low];
        assert_eq!(Governor::new(PARTIAL, Q::High), Err(LadderError::CeilingNotOnLadder));
        assert_eq!(Governor::new(&[Q::High], Q::High), Err(LadderError::TooShort));
    }

    #[test]
    fn n_down_consecutive_over_observations_step_down_exactly_once() {
        // mado's `n_down_consecutive_over_frames_step_down_once`.
        let mut g = gov();
        let before = feed(&mut g, Zone::Above, DOWN_AFTER - 1);
        assert_eq!(before, Step::Hold { state: Q::High }, "must not move early");
        assert_eq!(g.quality(), Q::High);

        let at = g.advance(Zone::Above);
        assert_eq!(at, Step::Stepped { from: Q::High, to: Q::Medium });
        assert_eq!(g.quality(), Q::Medium);

        // and the streak reset, so it does not cascade on the next tick
        assert_eq!(g.advance(Zone::Above), Step::Hold { state: Q::Medium });
    }

    #[test]
    fn m_up_consecutive_calm_observations_step_up_clamped_to_the_ceiling() {
        // mado's `m_up_consecutive_calm_frames_step_up_clamped_to_ceiling`.
        let mut g = gov().with_streaks(3, 5);

        // Setting a ceiling below the current rung pulls it down at once, so
        // the governor is already on Medium before any pressure arrives.
        g.set_ceiling(Q::Medium).unwrap();
        assert_eq!(g.quality(), Q::Medium);

        feed(&mut g, Zone::Above, 3);
        assert_eq!(g.quality(), Q::Low, "pressure sheds one rung per matured streak");

        feed(&mut g, Zone::Below, 5);
        assert_eq!(g.quality(), Q::Medium, "calm reclaims it");

        // At the ceiling now: further calm cannot exceed the user's maximum,
        // and the governor says so rather than silently holding.
        let at = feed(&mut g, Zone::Below, 5);
        assert_eq!(at, Step::AtCeiling { state: Q::Medium });
        assert_eq!(g.quality(), Q::Medium);
    }

    #[test]
    fn no_oscillation_on_mixed_or_noisy_observations() {
        // mado's `no_oscillation_on_mixed_or_noisy_frames`, the load-bearing
        // one: 1000 alternating observations must not move the tier AT ALL.
        //
        // ★ The governor MUST start mid-ladder, and this is not incidental.
        // A governor resting at its ceiling can only move down, so an
        // erroneously accumulating CALM streak climbs into `AtCeiling` and
        // leaves the tier unchanged — the test then passes for the wrong
        // reason and cannot see upward drift at all. Measured: with the
        // mutual streak reset deliberately deleted, a ceiling-resting version
        // of this test stayed green (34/34), i.e. it was blind to the very
        // mechanism it claims to prove. Starting on an interior rung gives the
        // tier room to move in BOTH directions, which is what makes the
        // assertion mean something.
        //
        // What this actually proves is the MUTUAL reset inside the Above/Below
        // arms (each zone clears the opposing streak), not the neutral reset —
        // the alternating signal never emits `Zone::InBand`. The neutral reset
        // has its own test.
        let mut g = gov().with_streaks(4, 6);
        feed(&mut g, Zone::Above, 4); // High -> Medium: room to move either way
        let start = g.quality();
        assert_eq!(start, Q::Medium);
        assert!(start != g.ladder().floor() && start != g.ceiling(), "must be interior");

        for i in 0..1000 {
            g.advance(if i % 2 == 0 { Zone::Above } else { Zone::Below });
        }
        assert_eq!(g.quality(), start, "an alternating signal must not move the tier");
        assert_eq!(g.streaks(), (0, 1), "and neither streak may accumulate across it");
    }

    #[test]
    fn the_shed_fast_reclaim_slow_asymmetry_stops_a_marginal_workload_hunting() {
        // Mechanism 2, and it was UNGUARDED until this test existed.
        //
        // Measured by red run: lowering UP_AFTER from 300 to DOWN_AFTER (30) —
        // destroying the 10x asymmetry the module docs call half the
        // no-oscillation guarantee — passed every other test in this file.
        // Both no-oscillation tests survived it, because an alternating or
        // neutral signal never builds a long enough one-sided streak to
        // notice. The asymmetry only shows up under a signal that is
        // one-sided, then briefly reverses — which is exactly the marginal
        // workload it exists to protect.
        //
        // Stated behaviourally rather than as `UP_AFTER >= 10 * DOWN_AFTER`,
        // which would only restate the constants and would still pass if the
        // arms stopped reading them.
        let mut g = gov();
        feed(&mut g, Zone::Above, DOWN_AFTER);
        assert_eq!(g.quality(), Q::Medium, "precondition: pressure sheds a rung");

        // The same number of CALM observations that sufficed to shed must not
        // suffice to reclaim. If it does, a workload sitting near its budget
        // gives the rung up and takes it straight back, forever.
        feed(&mut g, Zone::Below, DOWN_AFTER);
        assert_eq!(
            g.quality(),
            Q::Medium,
            "a calm no longer than the shed threshold must NOT reclaim the rung"
        );

        // It takes the full, much longer, calm streak.
        feed(&mut g, Zone::Below, UP_AFTER - DOWN_AFTER);
        assert_eq!(g.quality(), Q::High, "sustained calm does eventually reclaim it");
    }

    #[test]
    fn the_default_thresholds_are_asymmetric_in_the_safe_direction() {
        // The structural companion to the behavioural test above. Cheap, and
        // it names the direction: shedding is the safe move (it costs quality),
        // reclaiming is the risky one (it costs budget), so evidence for
        // reclaiming must be the more expensive of the two to accumulate.
        assert!(
            UP_AFTER > DOWN_AFTER,
            "reclaiming must demand strictly more evidence than shedding"
        );
    }

    #[test]
    fn a_neutral_observation_resets_both_streaks() {
        // Mechanism 3 in isolation — the subtlest of the three, and the one a
        // reimplementation is most likely to drop.
        let mut g = gov();
        feed(&mut g, Zone::Above, DOWN_AFTER - 1);
        assert_eq!(g.streaks(), (DOWN_AFTER - 1, 0));
        g.advance(Zone::InBand);
        assert_eq!(g.streaks(), (0, 0), "neutral must clear the over streak");

        feed(&mut g, Zone::Below, 10);
        assert_eq!(g.streaks(), (0, 10));
        g.advance(Zone::InBand);
        assert_eq!(g.streaks(), (0, 0), "neutral must clear the calm streak too");
        assert_eq!(g.quality(), Q::High, "and none of that moved the tier");
    }

    #[test]
    fn sustained_pressure_walks_down_to_the_floor_and_then_reports_at_floor() {
        let mut g = gov().with_streaks(2, 100);
        feed(&mut g, Zone::Above, 2);
        assert_eq!(g.quality(), Q::Medium);
        feed(&mut g, Zone::Above, 2);
        assert_eq!(g.quality(), Q::Low);
        feed(&mut g, Zone::Above, 2);
        assert_eq!(g.quality(), Q::Off);

        // Spent. The next matured streak must say so rather than reporting
        // Hold, which would read as "the system is fine".
        let spent = feed(&mut g, Zone::Above, 2);
        assert_eq!(spent, Step::AtFloor { state: Q::Off });
        assert!(!spent.changed());
    }

    #[test]
    fn lowering_the_ceiling_pulls_a_richer_state_down_immediately() {
        // A ceiling is a bound, not a target to drift toward: continuing to
        // render above one the user just lowered ignores an instruction.
        let mut g = gov();
        assert_eq!(g.quality(), Q::High);
        g.set_ceiling(Q::Low).unwrap();
        assert_eq!(g.quality(), Q::Low);
    }

    #[test]
    fn raising_the_ceiling_does_not_grant_quality_immediately() {
        // The complement: a raise only permits climbing, it does not perform
        // one. The calm streak still has to be earned.
        let mut g = gov();
        g.set_ceiling(Q::Low).unwrap();
        assert_eq!(g.quality(), Q::Low);
        g.set_ceiling(Q::High).unwrap();
        assert_eq!(g.quality(), Q::Low, "a raised ceiling is permission, not a step");
    }

    #[test]
    fn the_state_zone_matrix_is_total_and_never_leaves_the_ladder() {
        // The exhaustive matrix, enumerated from the two registries rather
        // than hand-listed, so a new rung or a new Zone widens it for free.
        for &start in Q::ALL {
            for &z in Zone::ALL {
                let mut g = gov().with_streaks(1, 1);
                g.set_ceiling(Q::High).unwrap();
                // drive to `start`
                while g.quality() != start {
                    g.advance(Zone::Above);
                }
                let out = g.advance(z);
                assert!(
                    g.ladder().contains(g.quality()),
                    "state left the ladder from {start:?} on {z:?}"
                );
                assert_eq!(out.state(), g.quality(), "Step::state must agree with the governor");
            }
        }
    }

    #[test]
    fn streak_counters_saturate_rather_than_overflow() {
        // A governor left running for a very long time under an unchanging
        // signal must not panic in debug. `with_streaks(u32::MAX, ..)` means
        // the threshold is never reached, so the counter climbs forever.
        let mut g = gov().with_streaks(u32::MAX, u32::MAX);
        for _ in 0..100 {
            g.advance(Zone::Above);
        }
        assert_eq!(g.streaks().0, 100);
    }

    #[test]
    fn a_zero_streak_threshold_is_clamped_to_one() {
        // A threshold of 0 would step on every observation, removing the
        // streak mechanism entirely — the opposite of this type's purpose.
        let g = gov().with_streaks(0, 0);
        let mut g2 = g;
        assert_eq!(g2.advance(Zone::Above), Step::Stepped { from: Q::High, to: Q::Medium });
    }

    #[cfg(feature = "zenmai")]
    #[test]
    fn the_machine_is_inert_on_a_neutral_tick_at_rest() {
        // zenmai's own proof harness: with both streaks at rest, a neutral
        // observation changes nothing and emits nothing.
        zenmai::assert_total_inert::<GovernorMachine<Q>>(gov(), Zone::InBand);
    }

    #[cfg(feature = "zenmai")]
    #[test]
    fn the_machine_emits_an_effect_only_when_something_happened() {
        use zenmai::Stateful;
        let mut s: Stateful<GovernorMachine<Q>> = Stateful::new(gov().with_streaks(2, 2));
        assert!(s.dispatch(Zone::Above).is_empty(), "a building streak is not an effect");
        let effects = s.dispatch(Zone::Above);
        assert_eq!(effects, vec![Step::Stepped { from: Q::High, to: Q::Medium }]);
    }
}
