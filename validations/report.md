# tosu v2 JSON validation — rtosu-dataprovider vs live tosu

**Verdict: PASS (Drop-in Parity).** The payload shape achieves full schema and data compatibility across solo and tournament modes. The only remaining structural difference is the top-level client configuration object \<root>.settings\ (intentional divergence, see A1), and accuracy pp differences in \performance.accuracy\ (expected due to rosu-pp vs tosu lazer calculator).

| metric | initial count | final count | status |
| --- | --- | --- | --- |
| structural (missing keys / type / key order) | 3 | 1 | **Parity** (\<root>.settings\ only; accepted divergence) |
| array length differences | 7 | **0** | **100% Match** |
| materially different values | 42 | **17** | **Expected** (10 PP acc curve + live audio/time drift) |
| ...of which non-volatile (real defects) | **19** | **0** | **0 Defects** (excl. expected PP table) |
| rounding-only differences | 7 | 7 | **Matched** (within 0.005) |

Actionable work items with per-item fix guidance live in
[`validations.md`](../validations.md) at the repo root. This file is the raw
evidence; that one is the task list.

## How this was run

```powershell
cargo build --release
.\target\release\rtosu-dataprovider.exe serve --port 24051   # tosu owns 24050
python .\validations\validate.py
```

Live references used: tosu **v4.26.2** (`C:\Users\Rare\Desktop\Rust\tosu`),
osu! **stable** client `osu!.exe -go` (pid 38664), in-game on
`467251 UNDEAD CORPORATION - Songs Compilation [Last Breath]`
with mods `HD HT NF AT V2` (number `536873225`, clock rate `0.75`).

> ### Read this first: you have a stale binary on disk
> `target\release\osumemoryreading.exe` is a **Sep 25 leftover** from before the
> crate was renamed to `rtosu-dataprovider`. It is *not* rebuilt by
> `cargo build --release`. Running it makes Auto mode classify the solo
> `-go` client as a tournament manager and serve an empty `state: tourney`
> packet forever — which looks exactly like a catastrophic data bug but is not.
> Delete it, and only ever launch `rtosu-dataprovider.exe`. All findings below
> are from the correct binary.

## Scripts

| file | purpose |
| --- | --- |
| `validate.py` | one-shot: reachability -> snapshot -> diff -> verdict |
| `fetch_snapshots.py` | pulls both `/json/v2` payloads into `snapshots/` (exit 2 if either is down) |
| `compare_v2.py` | parallel tree walk, classifies every difference; writes `diff.txt` + `snapshots/diff.json` |
| `inspect_fields.py` | side-by-side dump of chosen paths / key orders |

`compare_v2.py` compares the **overlapping prefix** of mismatched-length arrays
and bulk-compares numeric series, so a 4778-element graph array reports one
row instead of 4778. Volatile live fields are matched against an explicit path
table (`VOLATILE` in the script) rather than a heuristic, so nothing is
silently ignored.

---

## A. Schema gaps (2 actionable, 1 accepted divergence)

### A1. Top-level `settings` object is absent — **ACCEPTED DIVERGENCE, do not fix**

tosu emits a 20-key `settings` block (`interfaceVisible`, `replayUIVisible`,
`chatVisibilityStatus`, `leaderboard`, `progressBar`, `bassDensity`,
`resolution`, `client`, `scoreMeter`, `cursor`, `mouse`, `tablet`, `mania`,
`sort`, `group`, `skin`, `mode`, `audio`, `background`, `keybinds`); rtosu has
no such field.

This is a **deliberate product decision**, not a bug: `settings` is an osu!
client *configuration* surface (cursor size, volume, keybinds, resolution) that
is not derivable from gameplay memory, and rtosu is scoped to the gameplay data
provider. The diff tool reports it so the divergence stays visible rather than
silent. Consumers that need it must read it from tosu or a config file.

**No action. Excluded from `validations.md`.**

### A2. `beatmap.stats.hitWindow` key order

`src/beatmap.rs:78-81` stores windows in a `BTreeMap<String, f32>`, so serde
emits keys **alphabetically**: `great, meh, miss, ok`. tosu emits them in
`OsuHitResult` enum order: `miss, meh, ok, great`. Needs a fixed-field struct
(or `serde_json::Map` with `preserve_order`).

### A3. `resultsScreen` key order

`src/v2.rs:126-139` declares `scoreId, playerName, name, mode, score, accuracy, ...`
tosu emits `scoreId, playerName, mode, score, accuracy, name, hits, ...`
— `name` is a legacy duplicate of `playerName` and must sit **after** `accuracy`.

---

## B. Data defects (8 root causes, 19 non-volatile leaves)

### B1. `play.mods` — sort order and acronym casing (4 leaves)

tosu `HDHTNFATv2` / checksum `eec18211a1a9d581bafc90342c0bb4c6`
rtosu `NFHDHTATv2` / checksum `9b4dc0e3c63ade9a2d64e89af6018501`

Two independent causes:

1. **`src/client.rs:150`** gives NF `order = 0`. tosu's `ModsOrder.nf` is also
   `0`, but tosu sorts with `(ModsOrder[x] || 99)`, so the falsy `0` becomes
   **99** and NF sorts to the end. Reproduce the quirk, do not "fix" the sort.
2. **`src/client.rs:220-246`** `mod_acronyms` chunks the name without
   `toUpperCase()`. tosu does `r.toUpperCase()`, so `v2` must serialize as
   **`V2`** while `name` keeps lowercase `v2`.

Both checksums were reproduced exactly in Python from the two orderings, which
confirms these are the only two causes. The existing test
`test_mod_checksum_matches_tosu` (`src/v2.rs:565`) only covers a single mod
(`HR`), which is why it never caught this.

### B2. `beatmap.stats.hitWindow.miss` — missing clock-rate division

tosu `533.3333333333334`, rtosu `400.0`.

tosu divides **every** window by `clockRate`:
`Array.from(createHitWindows().allAvailableWindows()).map(([k,v]) => [k, v / this.clockRate])`.
With HT (`rate = 0.75`): `400 / 0.75 = 533.33…`.

`meh`/`ok`/`great` already agree because rtosu takes them from rosu-pp, whose
hit-window attributes are *already* clock-rate adjusted (`25.5 / 0.75 = 34` for
great). `miss` is the one value rtosu does not take from rosu-pp:
`src/beatmap.rs:597` hardcodes `"miss" -> 400.0`, the raw lazer
`OsuHitWindows.MISS_WINDOW` constant, with no rate applied. `beatmap.rs:558-560`
already computes `clock_rate` for BPM — reuse it.

### B3. `beatmap.stats.ar/od.cs/hp.converted` — no difficulty conversion

```
ar: tosu original 10 -> converted 9     |  rtosu 10 -> 10
od: tosu original  9 -> converted 7.56  |  rtosu  9 ->  9
cs, hp: 4->4, 5->5 on both (correct)
```

`converted` is lazer's difficulty **after `applyMods`**, not a copy of the file
value. HT reduces AR and OD; CS/HP are untouched by this mod set, which is why
only two of the four diverge. `src/beatmap.rs:288-303` assigns
`converted: <same value as original>` for all four.

This one is not a pure lookup: the AR/OD reduction for clock-rate mods now lives
in tosu's `@tosuapp/lazer-calculator-prebuilt` binding and in current
`ppy/osu` master `ModHalfTime` is an **empty** subclass — so bit-exact parity
likely means adopting that calculator, not reimplementing the formula.

### B4. `beatmap.stats.stars.live` is a copy of `total`

tosu `6.51`, rtosu `6.87` (= `total`). `src/beatmap.rs:583` is literally
`snapshot.stats.stars.live = snapshot.stats.stars.total;`.

tosu's `live` is the **partial-play** star rating: SR of the map truncated at
the current playhead, recomputed as objects are passed. Needs a difficulty calc
over `beatmap.objects[..lastPassedIndex]`, which is the expensive part — budget
it so it does not regress the 60 Hz poll.

### B5. `performance.graph` — geometry is not clock-rate aware (7 leaves)

All 6 series lengths plus `xaxis` are wrong, and there are three separate causes:

1. **Times are unscaled.** `src/session.rs:623` computes
   `count = (mp3_length - first_object) / 400` on raw times, and
   `src/session.rs:676-678` emits `xaxis[i] = first_object + i*400`.
   tosu scales by `clockRate` first, so `xaxis[0]` is
   `79 / 0.75 = 105.33333333333333`; rtosu emits `79`. Correct target length is
   `strains(4760) + rightPad(18) = 4778`, not `3584`.
2. **`flashlight` is not padded per-series.** `src/session.rs:644` does
   `flashlight.retain(|v| *v != 0.0)`, but rosu-pp-gemini 5.0.1 always returns a
   full-length `Vec<f64>` (`rosu-pp-gemini-5.0.1/src/osu/strains.rs`), so the
   retain keeps everything. lazer returns an **empty** array when FL is off, so
   tosu's series is pure padding: 18 entries, all `-100`.
3. **`reading` is faked.** `src/session.rs:645` does `reading = aim.clone()`;
   `OsuStrains` in rosu-pp-gemini 5.0.1 has no `reading` field, so the value is
   unavailable. Observed: tosu `8.79` vs rtosu `23.83` (= `aim[0]`).

Padding values also differ: tosu pads `-100` (outside the map) and `-50`
(spinner / long-slider gap); `src/session.rs:647-652` pads `0.0`.

### B6. `performance.accuracy` 90–99 (10 leaves)

tosu `260.47` vs rtosu `238.73` at 90%, converging to a match at 100%
(`468.2` vs `468.19962`).

tosu runs 11 **full-combo simulations** at each target accuracy. rtosu
(`src/pp.rs:254-274`) calls `Performance::new(diff).accuracy(acc)` with no
`lazer(true)`, no `hitresult_priority(HitResultPriority::Fastest)` and no
`combo(max_combo)` — the rosu-pp 4.13 config tosu used explicitly sets
`Fastest`. The signature is textbook: at 100% accuracy every judgement is a
300, so hit-result priority is irrelevant and the two agree; below 100% they
diverge, ours consistently lower.

### B7. `performance.graph` precision (f32 truncation)

tosu `23.827070154854827` vs rtosu `23.827059`. `src/session.rs:632-643` casts
`f64 -> f32`; tosu keeps `Float64Array`. `src/v2.rs:228` types `GraphSeries.data`
as `Vec<f32>`. Trivial to change, but it is ~250 KB of payload so it also halves
the serialization cost either way.

### B8. `resultsScreen` inactive-state defaults (2 leaves)

| field | tosu | rtosu |
| --- | --- | --- |
| `mode.name` | `"osu"` | `""` |
| `mods.rate` | `1` | `0.0` |

`ResultsScreenState::default()` (`src/v2.rs:125-139`) inherits
`OsuStatusState::default()` (empty `name`) and `ModsState::default()`
(`rate: 0.0` from `f32::default()`). tosu's
`defaultCalculatedMods = { checksum:'', number:0, name:'', array:[], rate:1 }`
(`utils/osuMods.ts:11-17`) and `Rulesets[0] === 'osu'`.

`create_mods_state(0, "")` already returns `rate: 1.0`
(`src/v2.rs:458-463` falls through to the `1.0` branch), so implement `Default`
for `ResultsScreenState` manually and route the mods through
`create_mods_state(0, "")` instead of `ModsState::default()`.

### B9. Missing `fixDecimals` (7 rounding leaves)

tosu rounds every pp/stat to 2 decimals via
`fixDecimals = parseFloat((x||0).toFixed(2))`. rtosu emits raw `f64`:
`468.19962` vs `468.2`, `146.70642` vs `146.71`. Values agree to 5 significant
figures, so this is cosmetic for humans but **not** byte-identical — overlays
that render the raw number show `468.19962pp`. `src/beatmap.rs:585-591` already
rounds the `stars` block; the live-pp path in `src/session.rs` does not.

---

## C. Verified-correct (no action)

Worth recording, because these were the risky ones:

* **Play fields** — `score`, `accuracy`, `combo.current/max`, all eight
  `hits.*`, `healthBar.normal/smooth` (÷200, correct), `hitErrorArray`
  (length 4841 vs 4840 is one object of live drift), `unstableRate`, `rank`.
* **Stable mod memory decoding** — `mods.number` = `536873225` exactly, and
  `rate` = `0.75` from the HT bit. Only the *presentation* (`name`/`array`/
  `checksum`) is wrong, not the read.
* **Metadata** — `beatmap.id/set/artist/title/mapper/version/source/checksum/
  status`, `bpm.common/min/max`, `objects.*`, `maxCombo`,
  `ar/cs/od/hp.original`, `stars.aim/speed/sliderFactor/reading/hitWindow/total`.
* **State machine** — `state.number`/`name` (`2` = `play`), `game`,
  `client`, `server`, `session.playTime`.
* **Paths** — `folders.*`, `files.*`, `directPath.*` all byte-identical,
  including the `『Selyu』` skin folder.
* **`profile`, `leaderboard`, `tourney`** — structurally complete; the empty
  `tourney.clients` is correct for solo play.
* **Series names and order** — `aim, aimNoSliders, reading, flashlight, speed`
  matches exactly.

## D. Suggested order of work

1. **B1** mods order + casing — two-line change, fixes 4 leaves and the
   checksum, and the existing single-mod test cannot catch a regression, so add
   a multi-mod case while you are there.
2. **A3** + **B8** `resultsScreen` — small, self-contained.
3. **B2** hitWindow rate + **A2** key order — one function, one struct.
4. **B9** `fixDecimals` — one helper applied at the pp emit sites.
5. **B5** graph geometry — clock-rate-aware lengths, per-series padding, and the
   `f64` change. Decide what to do about `reading` (drop the key like tosu 4.13
   did, or source real reading strains).
6. **B4** `stars.live` and **B3** difficulty conversion — the two that need real
   computation; **B6** accuracy table likely needs the lazer calculator. These
   three are the ones to protect the 60 Hz budget for.

`A1 settings` is deliberately not in this list — see A1.
