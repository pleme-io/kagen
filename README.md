# kagen (加減)

*"Adjustment by degrees; the right amount."*

The half of a homeostasis loop that selects a **named quality rung** — a
dead-band classifier, an ordered ladder, and a governor whose hysteresis makes
tier oscillation structurally hard.

Dependency-free, `Copy`, allocation-free on the hot path. Built to be ticked
inside a render loop.

```rust
use kagen::Governor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quality { Off, Low, Medium, High }
const ALL: &[Quality] = &[Quality::Off, Quality::Low, Quality::Medium, Quality::High];

let mut g = Governor::new(ALL, Quality::High)?;
let step = g.tick_frame(15_000, 16_666);   // 15ms spent of a 16.6ms budget
if step.changed() { /* push the new rung at the renderer */ }
# Ok::<(), kagen::LadderError>(())
```

## Why it exists

The same classifier had been written twice in the fleet, in two repos, over two
different quantities, with a **byte-identical** upper threshold — and nobody had
noticed:

```text
breathe-control   util = working_set / current_limit
                  util > 0.85  -> grow ;  util < 0.70  -> shrink ;  else Hold

mado              frac = frame_us / budget_us
                  frac > 0.85  -> over ;  frac < 0.60  -> calm   ;  else neutral
```

One measures bytes against a memory limit, the other microseconds against a
frame budget. `kagen` owns that function once, so the number has one owner
rather than a constant in each repo beside a comment explaining the coincidence.

## What it is not

It is **not** a `breathe_control::ControlLaw`, and the reasons are in the crate
docs. Briefly: the actuator sits on the other side of the ratio (breathe grows
the *denominator*; a tier governor sheds the *numerator*), so reusing the law
would inherit its vocabulary while quietly discarding its safety proof. The
codomain is a closed named ladder rather than a scalar, and the hysteresis needs
inter-tick state that a deliberately memoryless `propose(&self, …)` has nowhere
to hold.

So the two are **siblings** that share a classifier. The dependency edge is
pointed so `BandLaw::propose` can adopt `kagen::zone` and delete its copy.

## Provenance

Generalized from `mado`'s `ux::ambience_governor`, which has run in a live
render loop against one knob. This crate makes it reusable, names the parts, and
adds guards for the mechanisms that turned out to have none.

Every anti-oscillation mechanism was verified by **deleting it and confirming a
test goes red** — a test that has never failed is decoration, not evidence. That
exercise found two real defects in the tests as first written:

- The 1000-tick no-oscillation test must start the governor **mid-ladder**.
  Resting at its ceiling the governor can only move down, so an erroneously
  accumulating calm streak climbs into `AtCeiling` and the tier never moves —
  the test passes for the wrong reason and is blind to upward drift entirely.
- The **shed-fast / reclaim-slow asymmetry was unguarded**. Collapsing
  `UP_AFTER` (300) to `DOWN_AFTER` (30) passed every other test in the file,
  because neither an alternating nor a neutral signal ever builds a one-sided
  streak long enough for the asymmetry to matter.

## Honest tiers

In `selo`'s vocabulary, not rounded up:

| invariant | tier |
|---|---|
| a duplicate rung reaches a governor | parse-time-rejected |
| a ceiling that is not a rung reaches a governor | parse-time-rejected |
| the governor leaves the ladder | truly-unrep |
| "nothing left to give" reported as "nothing to do" | only-mitigated (C1) |
| the ladder is declared out of cost order | only-mitigated (C1) |
| the tier oscillates | only-mitigated (C1) |

The measurement feeding the classifier carries its own ceiling and it belongs to
the caller: a CPU-side frame time cannot observe a GPU-bound stall, so a
consumer measuring that way is `only-mitigated (C2)` on the budget axis however
good the hysteresis above it is. A consumer that owns a presentation clock can
do better. Neither is this crate's claim to make.

## Features

- `zenmai` — `impl zenmai::Machine for GovernorMachine<T>`, so the governor
  participates in the fleet's shared reducer vocabulary (`Stateful`, `Driver`,
  `assert_total_inert`). Off by default: `Machine::step` returns `Vec<Effect>`,
  and `Governor::advance` is the allocation-free call for a render loop.

MIT.
