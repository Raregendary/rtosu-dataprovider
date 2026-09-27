# Audit decision record

Verdicts on the 21 items in `fix.md`, plus the claims that were rejected or
corrected along the way. `fix.md` numbers jump from FIX-005 to FIX-021 and back;
ten IDs (006, 009, 012, 013, 015-020) are absent from the file. This pass covers
the 21 items that are present.

`validations.md` is a separate, already live-verified tosu-parity audit with its
own "do not fix these" list. None of its items were reopened here.

## Verdict summary

| Item | Verdict | Commit |
| --- | --- | --- |
| FIX-001 | Rejected — tosu quirk, current code is correct | — |
| FIX-002 | Fixed | `8deca70` |
| FIX-003 | Fixed | `bcffab3` |
| FIX-004 | Fixed; first claim rejected | `698fdc2` |
| FIX-005 | Rejected — `beatmap.stats` has no `pp` | — |
| FIX-007 | Fixed, but the stated mechanism does not apply | `9e62408` |
| FIX-008 | Deferred — architectural, blocked | — |
| FIX-010 | Rejected | — |
| FIX-011 | Fixed | `8f3d1ae` |
| FIX-014 | Fixed | `8deca70` |
| FIX-021 | Fixed | `add588c` |
| FIX-022 | Fixed; offset-independent | `52aa60c` |
| FIX-023 | Fixed | `714acc6` |
| FIX-024 | Fixed | `bcffab3` |
| FIX-025 | Rejected as specified; dead code deleted | `9e62408` |
| FIX-026 | Fixed, scope corrected against tosu source | `fb8221b` |
| FIX-027 | Fixed | `8deca70` |
| FIX-028 | Fixed | `9e62408` |
| FIX-029 | Taiko fixed; catch and mania blocked | `3e2773d` |
| FIX-030 | Fixed | `8f3d1ae` |
| FIX-031 | Rejected | — |

Plus one commit that is not an audit item: `88cf30d`, which makes the crate
compile under `--no-default-features` so the per-unit gate could run in both
feature configurations.

## Rejected items

### FIX-001 — `mod_acronyms` chunking is a tosu quirk, not a bug

`fix.md` asks for `mod_acronyms` to split on discrete acronym tokens so `10K`
survives. It should not. tosu builds `mods.array` by chunking the formatted
name two characters at a time
(`references/tosu/packages/tosu/src/utils/osuMods.ts`:
`name.match(/.{1,2}/g).map(r => ({ acronym: r.toUpperCase() }))`), and
`validations.md` §1 already reproduces tosu's own MD5 from that form. A token
split would change both `mods.array` and `mods.checksum` for every map with a
mod whose acronym is not exactly two characters, so this would be a parity
regression traded for a cosmetic one. Pinned by
`v2::tests::test_mod_checksum_matches_tosu` and by
`the_live_validated_mod_mask_still_formats_identically` in `client.rs`.

### FIX-005 — `beatmap.stats.pp` is correctly skipped

tosu's v2 payload has no `pp` field under `beatmap.stats`. PP lives at
`play.pp.{current, fc, maxAchievedThisPlay}`. `#[serde(skip)]` is right, and
`the_parsed_bpm_bases_stay_out_of_the_beatmap_json` now asserts the serialized
`bpm` object stays exactly the four documented fields.

### FIX-010 — `read_dotnet_string` cannot produce a torn string

`src/process.rs` reads the .NET length header and returns an error when the
length exceeds its ceiling, rather than truncating. The described failure mode
— a silently truncated string — is not reachable. A larger ceiling is a
reasonable memory question for unusually long strings, but it is a different
item and not a correctness fix.

### FIX-025 — osu! has no "F" grade

`TODO.md:14` lists the grades as `SS, S, A, B, A, C, D`; a fail reports D.
Adding `"F"` would diverge from both osu! and tosu. `calculate_tosu_grade` is
left alone. The audit's underlying observation — that `calculate_grade` was the
only code that produced an F, and that it was reachable only from its own test —
was correct, so the function and its test were deleted in `9e62408` to stop the
next pass re-raising this.

### FIX-031 — `write_buf` does not run per tick

`src/logging.rs` formats the date only when a record is actually emitted, not on
every poll. The allocation is real but it is bounded by the log rate, and it
costs a mutex-free `Local::now()` on a path that is already behind a lock and
doing disk I/O.

## Items fixed against a corrected scope

### FIX-007 — the poisoning mechanism cannot occur in the shipped binary

`Cargo.toml:73` sets `panic = "abort"`, so in the release binary a panic aborts
the process and a mutex is never left poisoned. The `.expect(...)` sites the
audit named in `client.rs:182, 212, 335` are `?`-propagating `Result` returns
rather than mutex locks, and the mutex sites that do exist are in `pp.rs` and
`session.rs`. Those were changed to
`unwrap_or_else(std::sync::PoisonError::into_inner)` anyway, since the dev and
test profiles do unwind and the change is free.

### FIX-004 — the object-counter desync claim is wrong

`values[3].parse::<u32>().unwrap_or(0)` yields a type matching no bit, so a
malformed line contributed to no counter and was already excluded from the
`objects.total` sum. It could, however, widen `first_object`/`last_object`, which
is the real defect and is what was fixed. Under the `pp` feature a separate
discrepancy remains: `objects.total` comes from `map.hit_objects.len()` via
rosu while the per-type counts come from the file loop, so rosu may parse a line
the loop skipped. That is pre-existing and untouched.

### FIX-026 — tosu does not clear on the way to the main menu

`fix.md` says to clear on transitions from state 2 or 7 back to 0 or 5. There is
no `updatePlayScores(memory, isMenu)` in tosu; the v2 play block is built
unconditionally by `buildPlay` and all gating is in the poll loop. In
`src/instances/osuInstance.ts`, `case GameState.menu` does nothing, so the main
menu keeps the last play **frozen**, while song select calls `gameplay.init()`
and `resultScreen.init()`. Clearing on 2→0 or 7→0 would have been a new parity
regression. The reset is gated on a `play_state_dirty` latch mirroring tosu's
`isDefaultState`, so it fires on song select and on tosu's `default:` arm and
never in the main menu. `play.accuracy` is restored to 100 rather than 0, which
is what `GameplayState.init` does.

### FIX-029 — catch and mania are blocked on the dependency

`rosu-pp-gemini 5.0.1` exposes:

```rust
// src/catch/attributes.rs
pub struct CatchDifficultyAttributes {
    pub stars: f64, pub preempt: f64,
    pub n_fruits: u32, pub n_droplets: u32, pub n_tiny_droplets: u32,
    pub is_convert: bool,
}

// src/mania/attributes.rs
pub struct ManiaDifficultyAttributes {
    pub stars: f64, pub n_objects: u32, pub n_hold_notes: u32,
    pub max_combo: u32, pub is_convert: bool,
}
```

Neither has a single skill value or hit window. The taiko arm is implemented;
the other two keep their defaults, pinned by
`catch_and_mania_keep_the_default_breakdown` so osu!std numbers cannot leak
into them later. Unblocking needs a newer difficulty calculator.
`validations.md` §10 records the same dead end for the `reading` strain series.

The taiko arm's values come from rosu-pp rather than from the game. That is a
divergence from tosu, which reads osu!'s own lazer difficulty object and hit
windows, but this crate reads osu! **stable** memory where those are not
available. The osu!std arm already had that property, so the two are now
consistent.

### FIX-028 — the panic half cannot be tested under `panic = "abort"`

The RAII guard covers the real defect, which was the `.ok()` on
`thread::Builder::spawn`: a failed spawn returns the closure in its error, so
the hand-written key removal never ran. `fix.md`'s proposed test, triggering a
panic in `compute_chunks`, is unwritable here because the release profile
aborts rather than unwinding. The two tests that were written instead pin the
contract directly: a second acquire is refused while the key is held, and
dropping the guard releases it — including the never-started-worker case.

### FIX-022 — no live process was available to confirm the field offset

A .NET `List<T>` is `{ _items, _size, _version }` at `+0x0/+0x4/+0x8`; whether
the i32 at `+0xC` is `_size` or `_version` needs a memory dump against a running
osu! client, and no osu! process was running on this machine. No offset was
guessed and the `+0xC` read was abandoned rather than changed. The fix is
offset-independent: the walk is bounded by the `_items` array length and stops
at the first null slot, and change detection uses a fingerprint of the newest
message (object address, content string address, that string's length, and its
index). That is correct under either reading, and a cache hit now costs five
small reads instead of a 500-slot walk. See the follow-up on `+0xC` below.

## Follow-ups found but not fixed

1. **`play.rank` in song select — confirmed to already match.** A previous
   revision of this file claimed tosu emits `'X'` here (from
   `utils/calculators.ts:145-150`) while we emit `""`. Live verification against
   a running tosu shows **both report `""`**, so there is no divergence. The
   reason is the same `!isDefaultState` latch as FIX-026: `gameplay.init()` only
   runs when leaving gameplay, so a client that was never in a map keeps the
   initialised `""`. No action needed; the earlier claim was wrong.
2. **`play.pp.fc` and `play.pp.maxAchievable` in song select — real, pre-existing
   divergence.** tosu reports **0** for `play.pp.fc`, `play.pp.maxAchievable` and
   all four `play.pp.detailed.fc.*` values; rtosu reports the highlighted map's
   FC PP (308.0 in the observed case). Identical on `main`, so this branch did
   not cause it. Source: `resetAttributes()` sets `maxAchievable: 0.0`, and
   `maxAchievable` is never assigned anywhere else in `beatmap.ts`, so tosu's
   value is always 0; `fcPP` is `this.ppAcc[100] || 0.0`, which is 0 whenever
   tosu's accuracy sweep has not run for the selected map. **Left unfixed
   deliberately** — see the note below the table.
3. **`overlays::resolve_within` double-decodes.** It percent-decodes a path Axum
   has already decoded. It fails safe and the traversal tests call it directly,
   so it was left out of FIX-030. Practical consequence: an *asset* inside a
   folder whose name or path contains a literal `%` will not resolve.
4. **`parse_spectate_client_arg` has the same quoting weakness FIX-003 fixed.**
   It does a raw `find` on the lowercased string. A `CommandLineTokens` now
   exists next to it and could replace this, but the tournament-flag semantics
   were the audited issue and this was left for a separate unit.
5. **The `+0xC` identity is still unknown.** `client.rs::read_hit_errors_arc`
   also reads that field for `MAX_HIT_ERRORS` truncation, so it inherits the same
   uncertainty. Resolving it needs a running osu! client and a `jdb`/WinDbg
   session, or `rosu-mem`'s offset table.
6. **`TournamentSession` has no retry bound on an unresolved beatmap.**
   `SoloSession` has `BEATMAP_RESOLVE_RETRIES`; the tournament path re-reads
   every tick until the snapshot resolves. Pre-existing and unchanged.
7. **`play_state_dirty` is not reset when a process is lost and reattached.**
   Harmless (the latch only causes an extra clear on the first poll in a new
   state), but it is a small leak.
8. **`objects.total` vs the per-type sum under `pp`.** See FIX-004 above.
9. **`beatmap.time.mp3Length` is 1 ms low.** Observed tosu 119433 vs rtosu
   119432. The value is a `f64` audio length truncated to `i32`; the audit does
   not mention it and it predates this branch. One line, if it matters.
10. **rtosu emits no `settings.*` object at all.** tosu populates 72 leaves
    there (keybinds, volume, resolution, skin, mode, sort, group, ...). rtosu
    omits the whole subtree. This is a feature gap rather than a defect, and it
    is the single largest source of byte-level divergence from tosu.
11. **86 pre-existing clippy style findings**, all `collapsible_if`,
    `field_reassign_with_default`, `map_or` → `map_or_else` and similar. Cleaning
    them is a separate style pass; this work only held the count at 86 in both
    feature configurations.
12. **Pre-existing rustfmt diff at `benches/micro.rs:110`.** Left alone to keep
    the audit commits free of unrelated churn; `cargo fmt --check` reports this
    and nothing else.

### Why `play.pp.fc` was left as-is

Matching tosu here means rtosu would report `0` in song select, discarding a
number an overlay can genuinely compute and that osu! itself knows. That is a
product decision — parity versus a strictly more useful field — and it predates
this branch, so it is reported rather than changed unilaterally. The decision
should be made once, deliberately, and then pinned by a test either way.


## Validation performed

### Static gates

- `cargo test` — 153 passed, 0 failed
- `cargo test --no-default-features` — 138 passed, 0 failed
- `cargo clippy --all-targets --all-features` — 86 findings before, 86 after
- `cargo clippy --no-default-features --all-targets` — 68 findings before, 68 after
- `cargo build --release` — clean
- `cargo fmt --check` — only the pre-existing `benches/micro.rs:110`
- `cargo doc --no-deps` — no warnings

### Live tosu parity, three-way

Run against a live osu! stable client (pid 26360) and a live tosu on
`127.0.0.1:24050`, with osu! idle in **state 5 (`selectPlay`)** and one map
selected. Both this branch and `main` were built in release and served on
separate ports, and all three packets were compared leaf by leaf.

```
leaves compared outside `performance` : 241
  main    diverges from tosu : 81
  branch  diverges from tosu : 81
  REGRESSIONS introduced by the branch  : 0
  divergences FIXED by the branch       : 0
  still diverging, identical on both    : 81
```

**Zero regressions.** All 81 remaining divergences are byte-identical between
`main` and the branch, so none of them was introduced here. They break down as
72 leaves of `settings.*` that rtosu does not emit at all (see follow-up 10),
6 leaves of `play.pp` (follow-up 2), and `beatmap.time.mp3Length` 1 ms low
(follow-up 9). `session.playTime` advances between two sequential reads and is
excluded as inherently volatile.

The values this pass specifically touched all match tosu **exactly**:

| field | tosu | branch |
| --- | --- | --- |
| `beatmap.stats.stars` (aim/speed/sliderFactor/reading/hitWindow/total) | 3.15 / 2.77 / 0.98 / 0.99 / 28.5 / 6.06 | identical |
| `beatmap.stats.hitWindow` (miss/meh/ok/great) | 400 / 115.5 / 71.5 / 28.5 | identical |
| `beatmap.stats.bpm` (realtime/common/min/max) | 200 / 200 / 200 / 200 | identical |
| `state.number` / `state.name` | 5 / `selectPlay` | identical |
| `beatmap.id` / `set` / `title` | 2964306 / 1404277 / Toono Gensou Monogatari | identical |
| `beatmap.time.firstObject` / `lastObject` / `live` | 1134 / 112584 / 75533 | identical |
| `play.rank.current` / `maxThisPlay` | `""` / `""` | identical |
| `play.pp.current` / `maxAchieved` | 0 / 0 | identical |

The matching BPM row is direct evidence for the FIX-002 fix: the clock-rate
conversion is now idempotent and a repeat call no longer double-scales.
The matching `stars` and `hitWindow` rows are the osu!std arm, which FIX-029
deliberately left untouched.

`compare-tosu` also ran, but with no tournament active it compares tosu's zeros
against rtosu's uninitialised tournament memory, so its `ipcState`/`points`/
`score` DIFF lines are not a parity signal.

### Performance

**Steady-state poll cost — the number that matters for the tick budget:**

```
branch  n=18  min=31 µs  median=32 µs  mean=32-33 µs
main    n=18  min=31 µs  median=32 µs  mean=31-34 µs
```

Measured with the tool built for it (`compare-tosu`, 18 interleaved samples per
binary against the same live process). Identical, and at ~0.3% of a 10 ms tick
budget. No change.

**Criterion micro-benchmarks — not able to support a claim either way.** The
first branch-vs-main comparison appeared to show the branch slower by a median
of +6.3%, but untouched control benchmarks moved by the same amount (including
`scoring/calculate_tosu_grade`, which this branch does not touch, at +20%). A
null control settled it — the **same binary benchmarked twice**:

```
NULL CONTROL  branch-run2 vs branch-run1 (identical code)
  n=33  median=-7.00%  mean=-6.11%  stdev=7.42%  |max|=25.4%

TEST         branch-run1 vs main-run1
  n=33  median=+6.30%  mean=+8.10%  stdev=11.22% |max|=50.0%
```

Run-to-run noise on *identical* code (−7.0% median, 25.4% worst case) is the
same size as the entire branch-vs-main signal (+6.3% median, 50% worst case),
with the opposite sign. `scoring/calculate_accuracy` — one of the few functions
this pass actually changed — measured +12.59% against main but **−12.69% against
itself** in the null control, a 25-point swing for unchanged code. The
measurement is not sensitive enough to attribute any of this to the branch, and
the noise is not attributable to it either. Criterion's own within-run detection
reported "No change in performance detected" for the touched benchmarks.

Anything stronger needs a pinned host with CPU frequency locking and more
samples per bench; the `target/criterion` baselines already in the tree are from
an unknown build and should not be trusted as a reference.

