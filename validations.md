# Validation follow-ups — tosu v2 parity

Actionable list produced by a live tosu-vs-rtosu `/json/v2` diff
(`validations/report.md` holds the raw evidence; `validations/*.py` is the
harness). Every item below was observed on a running osu! stable client, not
inferred from docs.

**Not on this list:** the missing top-level `settings` object. That is an
intentional divergence — `settings` is client configuration (cursor, volume,
keybinds, resolution), not gameplay memory, and rtosu is scoped to the
gameplay data provider. Do not "fix" it.

## Ground rules for whoever picks this up

* **Re-validate every change** with `python validations/validate.py`. Exit 0
  means parity. `validations/diff.txt` is the human-readable diff,
  `validations/snapshots/diff.json` the machine-readable one. Both payloads are
  also dumped to `validations/snapshots/{tosu,rtosu}.json` for
  `python validations/inspect_fields.py <path>` inspection.
* **Do not run `target\release\osumemoryreading.exe`.** It is a stale Sep-25
  leftover binary that is not rebuilt by `cargo build --release`. It misdetects
  Auto mode and serves an empty `state: tourney` packet, which looks like a
  catastrophic regression. Only `rtosu-dataprovider.exe` is current.
* tosu must be on `:24050` and rtosu on a different port, e.g.
  `rtosu-dataprovider serve --port 24051`.
* **Preserve the 60 Hz poll budget.** Items 8, 9 and 11 add real computation to
  a 16 ms budget. Measure before and after (`tournament-watch`, or the `instr`
  feature) and prefer caching/throttling over per-frame recalculation.
* Several items involve **deliberately reproducing a tosu quirk**. Where marked
  "replicate the quirk", do not write the *correct* algorithm — write tosu's.
  Add a comment saying the upstream behaviour is intentional and why.

## Priority order

Cheap and self-contained first, expensive and uncertain last. IDs are stable;
reference them in commit messages.

| # | item | effort | risk | verdict |
| --- | --- | --- | --- | --- |
| 1 | `play.mods` order + acronym casing | S | low | fix |
| 2 | `resultsScreen` inactive defaults | S | low | fix |
| 3 | `resultsScreen` key order | S | low | fix |
| 4 | `hitWindow.miss` clock-rate | S | low | fix |
| 5 | `hitWindow` key order | S | low | fix |
| 6 | `fixDecimals` 2-decimal rounding | M | low | fix |
| 7 | graph series `f64` precision | S | low | fix |
| 8 | graph geometry + padding | M | med | fix |
| 9 | `flashlight` series semantics | S | low | fix |
| 10 | `reading` series is fabricated | S | low | decide, then fix |
| 11 | `stars.live` is a copy of `total` | L | med | fix |
| 12 | `ar/od/cs/hp.converted` | L | med | investigate first |
| 13 | `performance.accuracy` 90–99 | L | med | investigate first |

---

### 1. `play.mods` — sort order and acronym casing — **FIX**

Four leaves wrong: `name`, `array[0..2]`, `array[4]`, and therefore `checksum`.

```
tosu  name "HDHTNFATv2"  array [HD, HT, NF, AT, V2]  checksum eec18211a1a9d581bafc90342c0bb4c6
rtosu name "NFHDHTATv2"  array [NF, HD, HT, AT, v2]  checksum 9b4dc0e3c63ade9a2d64e89af6018501
```

Mod set: `number = 536873225` = NF(1) + HD(8) + HT(256) + AT(2048) + V2(1<<29).
Both sides decode the *number* correctly — only the presentation is wrong.

**Cause 1 — NF sort position.** `src/client.rs:150` gives NF `order = 0`.
tosu's table (`tosu-master/packages/tosu/src/utils/osuMods.types.ts`,
`ModsOrder`) also has `nf: 0`, but tosu sorts with
`(ModsOrder[x.toLowerCase()] || 99) - (ModsOrder[y.toLowerCase()] || 99)`
(`utils/osuMods.ts:19-46`), and `0` is falsy, so NF's effective key becomes
**99** and it sorts to the end. `nf` is the *only* entry in `ModsOrder` with the
falsy value `0`; every unlisted mod also gets 99, and the sort is stable, so
those keep ascending-bit order. **Replicate the quirk:** NF's effective order
must be 99, not 0. The existing `VALUES` table already uses 99 for unlisted
mods, so a one-value change plus a comment is enough.

**Cause 2 — acronym casing.** `mod_acronyms` (`src/client.rs:220-246`) chunks
the mod name two characters at a time and pushes them verbatim. tosu does
`name.match(/.{1,2}/g).map(r => ({ acronym: r.toUpperCase() }))`
(`utils/osuMods.ts:88-91`), so the `v2` chunk must serialize as **`V2`** while
`name` keeps lowercase `v2` (tosu's `bitValues[29] === 'v2'`). That asymmetry is
intentional in tosu — do not normalise the name.

**Verification.** Both MD5s were reproduced in Python purely from the two array
orderings, which proves these are the only two causes:

```
md5('[{"acronym":"HD"},{"acronym":"HT"},{"acronym":"NF"},{"acronym":"AT"},{"acronym":"V2"}]')
  = eec18211a1a9d581bafc90342c0bb4c6   <- tosu
```

`create_mods_state` (`src/v2.rs:442`) serialises with `serde_json::to_string`,
which is already compact and matches JS `JSON.stringify`, so no change needed
there. **Add a regression test** for a multi-mod set: the existing
`test_mod_checksum_matches_tosu` (`src/v2.rs:565`) only covers single-mod `HR`,
which is exactly why this slipped through.

---

### 2. `resultsScreen` inactive-state defaults — **FIX**

| field | tosu | rtosu |
| --- | --- | --- |
| `resultsScreen.mode.name` | `"osu"` | `""` |
| `resultsScreen.mods.rate` | `1` | `0.0` |

`ResultsScreenState::default()` (`src/v2.rs:125-139`) derives `Default` and so
inherits `OsuStatusState::default()` (empty `name`) and `ModsState::default()`
(`rate: 0.0`, from `f32::default()`).

tosu uses `defaultCalculatedMods = { checksum:'', number:0, name:'', array:[],
rate:1 }` (`utils/osuMods.ts:11-17`) and `name: Rulesets[resultScreen.mode] || ''`
with `Rulesets[0] === 'osu'` (`common/enums/osu.ts:60`).

Note `create_mods_state(0, "")` already returns `rate: 1.0` — `src/v2.rs:458-463`
falls through to the `1.0` branch — so the fix is to stop going through
`ModsState::default()`. Implement `Default for ResultsScreenState` manually,
or build the inactive state explicitly. The same `mode.name` defaulting gap
will exist anywhere else `OsuStatusState::default()` is used for a known ruleset;
check `src/reader.rs` (`guest_profile` already hardcodes `"osu"`, so it is fine).

---

### 3. `resultsScreen` key order — **FIX**

```
tosu  scoreId, playerName, mode, score, accuracy, name, hits, mods, maxCombo, rank, pp, createdAt
rtosu scoreId, playerName, name, mode, score, accuracy, hits, mods, maxCombo, rank, pp, createdAt
```

`name` is a legacy duplicate of `playerName` and tosu emits it **after**
`accuracy`. Purely a field-declaration-order change in `src/v2.rs:126-139`. JSON
object order is not semantically meaningful, but byte-identical output is the
stated goal and some consumers diff or snapshot the raw payload.

---

### 4. `beatmap.stats.hitWindow.miss` is not clock-rate adjusted — **FIX**

```
tosu  533.3333333333334
rtosu 400.0
```

tosu divides **every** hit window by the clock rate
(`states/beatmap.ts` → `updateMapMetadata`):

```ts
hitWindow: Object.fromEntries(
    Array.from(this.beatmap.createHitWindows().allAvailableWindows())
        .map(([key, value]) => [uncapitalize(key), value / this.clockRate])
)
```

With HT (`rate = 0.75`): `400 / 0.75 = 533.33…`.

`meh` / `ok` / `great` already agree because rtosu takes those from rosu-pp,
whose hit-window attributes are *already* clock-rate adjusted (raw great
`25.5 / 0.75 = 34`). `miss` is the one value rtosu does **not** take from
rosu-pp: `src/beatmap.rs:597` hardcodes `"miss" -> 400.0`, the raw lazer
`OsuHitWindows.MISS_WINDOW` constant. `src/beatmap.rs:558-560` already computes
`clock_rate` for the BPM block — reuse it.

Raw lazer values for reference (`OsuHitWindows.cs`, OD 9): great
`floor(80 - 6*9) - 0.5 = 25.5`, ok `floor(140 - 8*9) - 0.5 = 67.5`, meh
`floor(200 - 10*9) - 0.5 = 109.5`, miss `400` (a constant, not OD-derived).
After `/0.75`: `34`, `90`, `146`, `533.33…`.

**Caveat:** tosu builds this from `allAvailableWindows()`, so the key set is
**ruleset-dependent** — osu!std emits 4 keys, mania emits a different set. The
current hardcoded 4-key `BTreeMap` is osu-std-only. Fixing the rate is in scope;
generalising to other rulesets is a separate call.

---

### 5. `beatmap.stats.hitWindow` key order — **FIX**

```
tosu  miss, meh, ok, great      (OsuHitResult enum order)
rtosu great, meh, miss, ok      (alphabetical, from BTreeMap)
```

`HitWindowState` (`src/beatmap.rs:78-81`) holds a
`BTreeMap<String, f32>`, so serde emits keys sorted. Replace with an explicit
field struct, or `#[serde(flatten)] pub values: serde_json::Map<String, f32>`
with the `preserve_order` feature. Note `ModEntry` already solves the same
problem in `src/client.rs:143`, so follow that pattern for consistency.

---

### 6. Missing `fixDecimals` (2-decimal rounding) — **FIX**

Eight leaves agree to 5+ significant figures and differ only in representation:

```
play.pp.fc                      468.2       vs 468.19962
play.pp.maxAchievable           468.2       vs 468.19962
play.pp.detailed.current.aim    146.71      vs 146.70642
play.pp.detailed.current.speed  109.3       vs 109.301704
play.pp.detailed.fc.aim         178.75      vs 178.75279
play.pp.detailed.fc.speed       134.43      vs 134.43373
play.pp.detailed.fc.accuracy     84.87      vs 84.86822
performance.accuracy.100        468.2       vs 468.19962
```

tosu rounds at the emit site with
`fixDecimals = parseFloat((x || 0).toFixed(2))`
(`tosu-master/packages/tosu/src/utils/converters.ts:19-20`).

`src/beatmap.rs:585-591` already rounds the `stars` block with a `round_value`
helper — reuse it at the live-pp emit sites in `src/session.rs` and in the
`performance.accuracy` table. Cosmetic for humans, but not byte-identical, and
overlays that render the raw number will show `468.19962pp`.

---

### 7. Graph series lose precision to `f32` — **FIX**

```
tosu  performance.graph.series[0].data[0] = 23.827070154854827
rtosu                                       23.827059
tosu  performance.graph.series[4].data[0] = 24.16359949478668
rtosu                                       24.163599
```

`src/session.rs:632-643` casts each strain `f64 -> f32`. tosu keeps
`Float64Array`. `GraphSeries.data` is typed `Vec<f32>` in `src/v2.rs:228` —
change to `Vec<f64>` and drop the casts.

Side benefit: the graph is ~250 KB of the payload, so `f64` costs a little
bandwidth but removes six lossy conversions per section per frame. Measure
serialization cost; `PrecomputedGraph` (`src/v2.rs:239`) already caches the
serialized string, so the risk is contained.

---

### 8. Graph geometry is not clock-rate aware — **FIX**

All five series lengths plus `xaxis` are wrong, which accounts for 6 of the 7
`ARRAY_LEN` diffs.

```
tosu  every series = 4778, xaxis = 4778
rtosu aim/aimNoSliders/reading/speed = 3584, xaxis = 3584
```

`src/session.rs:623` computes `count = ceil((mp3_length - first_object) / 400)`
on **unscaled** times, and `fit()` (`src/session.rs:647-652`) resizes every
series to that count with `0.0` padding. `src/session.rs:676-678` emits
`xaxis[i] = first_object + i*400`, also unscaled.

Verified formulas (checked against the live payload, `rate = 0.75`,
`firstObject = 79`, `lastObject = 1428173`, `mp3Length = 1433443`):

```
realStrainCount   = 4760                                 (raw rosu-pp vector length)
EMPTY_OFFSET_L    = floor((firstObj  / rate) / 400) = floor(105.33/400)     = 0
EMPTY_OFFSET_R    = ceil(((mp3Length / rate) - (lastObj / rate)) / 400)
                  = ceil(((1911257.33 - 1904230.67)) / 400) = ceil(17.57)  = 18
total             = 0 + 4760 + 18                                          = 4778
xaxis[i]          = (firstObj / rate) + i*400 = 105.33333333333333 + i*400  [verified]
```

The observed payload matches exactly: `-100` padding occupies indices
`4760..4777` in every real series, and `xaxis[0] == 105.33333333333333` while
rtosu emits `79`.

Note the raw rosu-pp vector is already clock-rate scaled internally, so do
**not** recompute the strain count — take `strains.len()` and add the two pads.

**Padding values.** tosu pads with `-100` (outside the map) and `-50`
(spinner / long-slider gap, `updateWithOffset` in `states/beatmap.ts`). Only
`-100` was observed in this snapshot (0 `-50` values — this map's strains
needed no gap padding), so **the `-50` rule is unverified here**. Find the
upstream definition before implementing it; do not guess.

---

### 9. `flashlight` series is not padded per-series — **FIX**

```
tosu  length 18,  every value -100   (empty strain array + right pad)
rtosu length 4760, real strain values
```

tosu's flashlight strain array is **empty** when the FL mod is absent (lazer
only instantiates the flashlight skill when FL is present), so the series is
pure padding. rosu-pp-gemini always returns a full-length vector, so
`src/session.rs:644`'s `flashlight.retain(|v| *v != 0.0)` does not empty it
(the values are small non-zero floats, not exact zeros). That retain is a
no-op and the series then bypasses `fit()`, so it also ends up the wrong length.

Fix: gate on the FL mod being present rather than on value inspection, and run
the series through the same pad helper as the others. Note the current code
also produces a `flashlight` series **longer** than `aim` (4760 vs 3584), which
is internally inconsistent and is a good signal to add a test for.

---

### 10. `reading` series is fabricated — **DECIDE, THEN FIX**

```
tosu  performance.graph.series[2].data[0] = 8.79273392041329
rtosu                                       23.827059   (== aim[0])
```

`src/session.rs:645` does `reading = aim.clone()`. The reason is structural,
not laziness: `rosu-pp-gemini 5.0.1`'s `OsuStrains`
(`~/.cargo/registry/src/*/rosu-pp-gemini-5.0.1/src/osu/strains.rs`) has only
`aim`, `aim_no_sliders`, `speed`, `flashlight` — **there is no `reading`
field**, so the real value is unreachable with the current dependency.

Emitting aim data under the name `reading` is actively misleading: any overlay
drawing the reading graph is drawing the aim graph. Pick one:

* **(a) Omit the `reading` series** when the value cannot be sourced. tosu 4.13
  did not emit `reading` at all, so consumers already handle its absence.
  Cheapest and most honest option.
* **(b) Upgrade/patch the pp dependency** to a build exposing reading strains,
  then compute it for real.
* **(c) Keep the key and emit zeros** — a third option, but it plots a flat
  line, which is arguably worse than omitting it.

Recommend (a) unless reading strains are specifically needed.

---

### 11. `beatmap.stats.stars.live` is a copy of `total` — **FIX, EXPENSIVE**

```
tosu  6.51      (partial play, ~12.7 min into a 23.8 min map)
rtosu 6.87      (== stars.total, the full-map rating)
```

`src/beatmap.rs:583` is literally:

```rust
snapshot.stats.stars.live = snapshot.stats.stars.total;
```

tosu's `live` is the star rating of the map **truncated at the current
playhead**, recomputed as objects are passed
(`currAttributes.stars`, fed from `gradualPerformance.nth(...)` /
`updateEditorPP` with `passedObjects = lastIndex(r => r.startTime <= playTime) + 1`).
It rises toward `total` as the play progresses, and is `0` before the first
object.

This needs a difficulty calculation over the passed-object prefix on every poll
— genuinely expensive, and it is the item most likely to hurt the 60 Hz budget.
tosu does it at 60 Hz, so it is achievable, but:

* Measure first (`tournament-watch`, or build with `--features instr` and set
  `RTOSU_INSTR=30`).
* Consider reusing the existing 10-object pp chunking already present in
  `src/pp.rs` (`gradual_pp_chunks`, default 100) instead of a fresh calculation
  per poll, and check the accuracy cost of that approximation.
* This is the same computation as item 13's problem, so consider bundling them.

---

### 12. `beatmap.stats.ar/od/cs/hp.converted` is a copy of `original` — **INVESTIGATE FIRST**

```
ar   tosu 10 -> 9      rtosu 10 -> 10
od   tosu  9 -> 7.56   rtosu  9 ->  9
cs   tosu  4 -> 4      rtosu  4 -> 4    (agrees, by luck of this mod set)
hp   tosu  5 -> 5      rtosu  5 -> 5    (idem)
```

`src/beatmap.rs:288-303` assigns `converted: <same value as original>` for all
four. `converted` is lazer's difficulty **after `applyMods`**, not a copy of the
file value. HT reduces AR and OD; CS and HP are untouched by this mod set, which
is why only two of the four diverge — **do not read that as "cs/hp are done"**,
they simply have not been exercised. A map with a difficulty-adjust mod, or a
tournament HR/HR+DT set, would expose them.

**Why this is flagged investigate-first rather than fix:** the reduction is not
reproducible from a documented formula. In current `ppy/osu` master,
`OsuModHalfTime` is an empty subclass of `ModHalfTime` and neither
`ModDoubleTime` nor `ModRateAdjust` implements `IApplicableToDifficulty`. The
AR/OD reduction lives in tosu's `@tosuapp/lazer-calculator-prebuilt` binding
(v4.26.2 replaced rosu-pp with a lazer napi binding for this). The observed
values fit `10 * 0.75 * 1.2 = 9.0` and `9 * 0.75 * 1.12 = 7.56`, but those
multipliers are **fitted to one data point, not cited** — do not hardcode them.
Determine whether the binding is adoptable, or whether a documented
`IApplicableToDifficulty` implementation can be ported.

---

### 13. `performance.accuracy` 90–99 is systematically low — **INVESTIGATE FIRST**

| acc | tosu | rtosu |
| --- | --- | --- |
| 90 | 260.47 | 238.73477 |
| 95 | 320.53 | 278.086 |
| 99 | 423.67 | 379.92075 |
| 100 | 468.2 | 468.19962 (rounding only) |

tosu runs 11 **full-combo simulations**, one per accuracy step
(`states/beatmap.ts` → `updateMapMetadata`). rtosu's
`calc_accuracy_table_from_diff` (`src/pp.rs:254-274`) does:

```rust
Performance::new(diff.clone()).accuracy(acc).calculate().pp()
```

with **no** `hitresult_priority` and **no** `lazer` flag. tosu's rosu-pp
configuration (`tosu-master/packages/tosu/src/states/beatmap.ts:450-455`,
verbatim):

```ts
const calculate = new rosu.Performance({
    mods: sanitizeMods(currentMods.array),
    accuracy: acc,
    lazer: this.game.client === ClientType.lazer,
    hitresultPriority: HitResultPriority.Fastest
}).calculate(this.performanceAttributes);
ppAcc[acc] = fixDecimals(calculate.pp);
```

`HitResultPriority` appears **nowhere** in this repo — confirmed by grep, so
the import and the builder call are both missing. Note tosu sets **no `combo`**
on this calculation; do not add one. Also note `lazer` is
`client === ClientType.lazer`, i.e. **`false` for a stable client** — do not
pass `lazer(true)`.

The signature is diagnostic: at 100% accuracy every judgement is a 300, so hit
result priority cannot matter and the two agree; below 100% they diverge, with
rtosu consistently lower. That points squarely at the
hit-object-to-judgement assignment rather than at the difficulty calculation.

**Version caveat, and why this is investigate-first.** The tosu actually running
on `:24050` is **v4.26.2**, which replaced rosu-pp with the
`@tosuapp/lazer-calculator-prebuilt` napi binding and computes this table as
`beatmap.calculatePerformance(attrs, { ...createScore(acc/100), maxCombo })`.
A different calculator means **exact parity is unreachable with rosu-pp** — the
`Fastest` fix above should narrow the gap substantially and is worth doing
regardless, but closing it completely means adopting the lazer binding. Decide
that trade-off explicitly rather than tuning constants until the numbers
coincide on this one map.

Cost note: this table is 11 full simulations, recomputed on beatmap change. It
is the most expensive single computation in the provider — verify the fix does
not regress the beatmap-change path, and re-measure the 60 Hz poll.

## Verified-correct — do not "fix" these

Recorded because they were the plausible-looking suspects:

* All `play.*` live fields: `score`, `accuracy`, `combo.current/max`, all eight
  `hits.*`, `healthBar.normal/smooth` (divide by 200 — correct),
  `hitErrorArray`, `unstableRate`, `rank`.
* Stable mod memory decoding: `mods.number == 536873225` exactly, `rate == 0.75`
  from the HT bit. Only presentation is wrong (item 1), not the read.
* Beatmap metadata: `id`, `set`, `artist`, `title`, `mapper`, `version`,
  `source`, `checksum`, `status`, `bpm.common/min/max`, `objects.*`,
  `maxCombo`, `ar/cs/od/hp.original`, `stars.aim/speed/sliderFactor/reading/hitWindow/total`.
* State machine: `state.number`/`name` (`2` = `play`), `game`, `client`,
  `server`, `session.playTime`.
* Paths: `folders.*`, `files.*`, `directPath.*` byte-identical, including the
  non-ASCII `『Selyu』` skin folder.
* `profile`, `leaderboard`, `tourney` structurally complete; the empty
  `tourney.clients` is correct for solo play.
* Graph series **names and order**: `aim, aimNoSliders, reading, flashlight,
  speed` matches exactly.
