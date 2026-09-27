<!--
  Verdicts added after the remediation pass. One row per item, with the
  commit that resolved it or the reason it was rejected. The reasoning and
  evidence live in audit.md at the repository root.

  15 fixed, 5 rejected on evidence, 1 partial (taiko only, rest blocked on a
  dependency), 1 open (FIX-008, not blocked after all — the plan's stated
  blocker was a misreading).
-->

# Comprehensive Bugfix & Hardening Plan (rtosu-dataprovider)

## P0 — Critical Correctness & Breaking Bugs (Fix Immediately)

### [x] FIX-001: mod_acronyms produces broken output for "10K" and drops trailing chars
> **REJECTED** — tosu builds `mods.array` by 2-char chunking (`utils/osuMods.ts`: `name.match(/.{1,2}/g)`). `validations.md` §1 reproduces tosu's own MD5 from this form, so a token split would change `mods.array` AND `mods.checksum` — a parity regression traded for a cosmetic one. Pinned by `test_mod_checksum_matches_tosu`.

- **File:** `src/client.rs` (lines 205–234)
- **Issue:** `mod_acronyms` formats the mods into a concatenated string via `format_mods(mods)` and slices the string 2 characters at a time (`while let (Some(a), Some(b)) = (chars.next(), chars.next())`). In `compute_format_mods`, key mod `1 << 25` is `"10K"` (3 characters). Grabbing 2 characters turns it into `"10"`, and the leftover `"K"` pairs with the first character of whatever mod follows (e.g. `"10K"` + `"NF"` becomes `["10", "KN"]`).
- **Implement:** 
  1. Refactor `compute_format_mods` to return a structured list of discrete acronym tokens (`&'static str`).
  2. Derive `format_mods` by joining that list, and derive `mod_acronyms` directly from the list of tokens without string-slicing.
- **Test:** `1 << 25` → `["10K"]`; `8 | (1 << 25)` → `["HD", "10K"]`; `64 | 512` → `["NC"]`.

---

### [x] FIX-021: Broken Accuracy Formula for Catch (Mode 2) and Mania (Mode 3)
> **FIXED** — `add588c`. Catch used score weights over an unweighted total (30000% on a perfect run); mania ignored geki/katu entirely and the caller discarded them. ScoreV2 MAX weight is 305 in *both* numerator and denominator (misses included), verified against ppy/osu — the wiki's page has an unbalanced bracket and cannot be read as 300 there. osu!/taiko pinned by a regression test.

- **File:** `src/client.rs` (`calculate_accuracy`, lines 360–392, and `read_result_screen_state`, line 426)
- **Issue:**
  - **Catch (Mode 2):** Accuracy is calculated as:
    ```rust
    (hit_300 as f64 * 300.0 + hit_100 as f64 * 100.0 + hit_50 as f64 * 50.0) / total * 100.0
    ```
    In Catch, hit counts are fruits and droplets, not score weights! Multiplying by 300 and then 100 blows accuracy up to **over 30,000%**.
  - **Mania (Mode 3):** Calculated as `(hit_300 as f64 / total) * 100.0`. This completely ignores `hit_geki` (MAX / rainbow 300s, which is the primary judgment in Mania) and `hit_katu` (200s)! In `read_result_screen_state`, `geki` and `katu` aren't even passed to `calculate_accuracy`, causing Mania accuracy to report near 0%.
- **Implement:**
  - Update `calculate_accuracy` signature to accept `(mode: i32, hit_300: i16, hit_100: i16, hit_50: i16, hit_miss: i16, hit_geki: i16, hit_katu: i16)`.
  - **Mode 2 (Catch):**
    ```rust
    let total = (hit_300 + hit_100 + hit_50 + hit_miss + hit_katu) as f64;
    if total == 0.0 { 100.0 } else { ((hit_300 + hit_100 + hit_50) as f64 / total) * 100.0 }
    ```
  - **Mode 3 (Mania):**
    ```rust
    let total = (hit_geki + hit_300 + hit_katu + hit_100 + hit_50 + hit_miss) as f64;
    if total == 0.0 { 100.0 } else {
        ((hit_geki as f64 * 300.0 + hit_300 as f64 * 300.0 + hit_katu as f64 * 200.0 + hit_100 as f64 * 100.0 + hit_50 as f64 * 50.0)
            / (total * 300.0)) * 100.0
    }
    ```
  - Update calls in `read_result_screen_state` and wherever `calculate_accuracy` is used.
- **Test:** Verify standard 100% accuracy for CtB with mixed fruits/droplets, and Mania with only Geki hits.

---

### [x] FIX-022: Tournament Chat Permanently Freezes When Channel Reaches Capacity
> **FIXED** — `52aa60c`. The `+0xC` field's identity (`_size` vs `_version`) is still unverified — no offset was guessed. The fix is offset-independent: the walk is bounded by the `_items` array length and stops at the first null slot, and change detection uses a fingerprint of the newest message. A hit now costs 5 small reads, not a 500-slot walk.

- **File:** `src/tournament.rs` (`read_tournament_chat`, lines 213–217)
- **Issue:**
  ```rust
  if let Some((cached_size, cached_msgs)) = cached_chat {
      if messages_size == cached_size && !cached_msgs.is_empty() {
          return Ok(cached_msgs.to_vec());
      }
  }
  ```
  Osu!'s `#multiplayer` channel has a fixed-capacity circular buffer (e.g. 100 or 500 messages). Once the chat fills up, old messages are evicted as new ones arrive, so `messages_size` stays constant (e.g. 100). The reader checks `messages_size == cached_size`, which evaluates to `true` forever. **No new chat messages will ever be read for the rest of the stream.**
- **Implement:** Do not rely exclusively on `messages_size`. Compare the pointer or string content of the last message in `messages_items`, or inspect the `.NET` `List._version` counter (offset `0xC` on x86). If the newest message differs, parse the full list.
- **Test:** Simulate a message list staying at size 100 with a new newest message; verify the reader outputs the new message.

---

### [x] FIX-023: Beatmap Pointer Re-use Bug in TournamentSession
> **FIXED** — `714acc6`. Both sessions now call one shared `beatmap_refresh_needed`, so the rule cannot drift apart again. The id is cached only alongside a resolved snapshot, or a mid-parse `id == 0` would pin the cache and the real map would never load.

- **File:** `src/session.rs` (`TournamentSession::poll`, line 144)
- **Issue:** `SoloSession` checks `(live_id > 0 && live_id != self.cached_beatmap_id)` because osu! stable reuses the same memory object when changing maps. `TournamentSession` **only** checks `beatmap_addr == client.cached_beatmap_ptr`. When tournament clients pick a new map reusing the same memory structure, `TournamentSession` misses the map change and serves the old map's metadata and PP for the rest of the match.
- **Implement:** Call `crate::beatmap::read_beatmap_id(memory, beatmap_addr)` inside `TournamentSession` and detect map changes if `live_id != client.cached_beatmap_id`.
- **Test:** Verify that when `beatmap_addr` remains identical but `read_beatmap_id` changes, a re-read is triggered.

---

### [x] FIX-024: Autoplay Flag (-go / /go) Still Present in session.rs
> **FIXED** — `bcffab3`. The clause is gone. Consequence is limited to *forced* tournament mode, where a lone `-go` process now yields an empty packet instead of a fabricated manager. Auto mode is unaffected — `reader.rs` already routed a lone `-go` to `SoloSession`.

- **File:** `src/session.rs` (`init_process`, line 560)
- **Issue:** While `is_tournament_manager_cmd` in `client.rs` was fixed, `session.rs` retained the old logic:
  ```rust
  let is_manager = is_tournament_manager_cmd(&command_line)
      || (!is_spectator && (command_line.contains("-go") || command_line.contains("/go")));
  ```
  `-go` is osu!'s autoplay flag. An osu! process launched with `-go` is misclassified as a tournament manager.
- **Implement:** Remove `|| (!is_spectator && (command_line.contains("-go") || command_line.contains("/go")))`. Only rely on the sanitized `is_tournament_manager_cmd`.

---

### [x] FIX-025: Failed Grade ("F") Never Set in calculate_tosu_grade
> **REJECTED (as specified)** — osu! has no "F" grade; a fail reports D (`TODO.md:14` lists `SS, S, A, B, C, D`). Adding it would diverge from osu! AND tosu. The audit's *other* observation was right, so the dead `calculate_grade` that was the only F producer was deleted in `9e62408`. `calculate_tosu_grade` left alone.

- **File:** `src/client.rs` (`calculate_tosu_grade`, lines 564–618)
- **Issue:** `calculate_tosu_grade` is used during live gameplay and result screens, but it does not receive `player_hp` and never checks for fail state. If a player fails, the grade remains `"D"`, `"C"`, `"B"`, or `"A"`. The separate `calculate_grade` function that did check `player_hp <= 0` is unused dead code.
- **Implement:** Pass `player_hp: f64` to `calculate_tosu_grade`. If `player_hp <= 0.0`, immediately return `"F"`.
- **Test:** Ensure passing `player_hp = 0.0` yields `"F"` across all game modes.

---

### [x] FIX-002: BPM Values Double-Scaled by Clock Rate When Stats Run Twice
> **FIXED** — `8deca70`. The parsed BPM now lives in a `#[serde(skip)]` `bpm_base` and the conversion is a pure function of (base, clock rate), so a repeat call cannot double-scale. The zero-base fallback keys off the base, which is what keeps it idempotent. A DT → nomod round trip was also broken (stranded at 1.5x) and is fixed.

- **File:** `src/beatmap.rs` (`populate_beatmap_statistics_with_diff`)
- **Issue:** Conditionally overwrites `bpm.realtime/min/max` based on "is it 0.0", and in the `else` branch multiplies the existing value by `clock_rate`. If called twice on the same snapshot, BPM is multiplied by `clock_rate` twice.
- **Implement:** Keep base/original BPM values immutable in the struct, and compute converted/live values strictly as `base * clock_rate`. Never multiply an already-scaled field.
- **Test:** Call `populate_beatmap_statistics_with_diff` twice in sequence on a DT beatmap and assert BPM values are scaled exactly 1.5x, not 2.25x.

---

### [x] FIX-003: is_tournament_manager_cmd Over-Matches "tournament" in Arguments
> **FIXED** — `bcffab3`. Quote-aware allocation-free tokenizer; a flag counts only as a whole token, up to an optional `=value`, with enclosing quotes stripped. `-tournamentx` and a bare `tournament` no longer match either.

- **File:** `src/client.rs`
- **Issue:** `lower.contains("tournament")` matches songs folders or file paths containing "tournament" (e.g. `C:\Games\tournament_pack\...`).
- **Implement:** Parse arguments into individual tokens. Check for exact flags `-tourney`, `/tourney`, `-tournament`, or `/tournament`. Do not perform raw substring matching across the entire unparsed command-line string.
- **Test:** Assert `osu!.exe "D:\Songs\tournament\map.osu"` evaluates to `false`.

---

## P1 — Reliability, Security & System Architecture

### [ ] FIX-008: Move Blocking reader.poll() Off the Tokio Async Worker Thread
> **OPEN — NOT blocked** — This is the one item still outstanding. The plan recorded it as blocked by an `Arc<Mutex<TosuReader>>`; that was a misreading — `reader` is a plain local `OsuReader` in `run_serve_loop`, and `ProcessMemory` already declares `unsafe impl Send + Sync` (`process.rs:576`). `reader_can_be_moved_to_another_thread` asserts `OsuReader: Send` at compile time. The `watch::channel` the fix needs already exists (`AppState.packet_rx`). Unimplemented, and not done here because it restructures the serve loop rather than fixing a defect.

- **File:** `src/main.rs` (`run_serve_loop`, lines 737–755)
- **Issue:** `reader.poll()` runs synchronously inside `tokio::select!` directly on the Tokio worker thread. It executes synchronous Windows memory scans (`ReadProcessMemory`, `VirtualQueryEx`), thread pool spawning, and disk I/O. When beatmaps change or processes attach, the async worker is starved, causing HTTP requests and WebSocket streams to stutter. Furthermore, `tokio::time::sleep` causes poll rate drift (60 Hz drops to ~45 Hz).
- **Implement:** 
  1. Spawn a dedicated OS thread (`std::thread::Builder::new().name("reader-poll")`) running a steady tick loop (using `std::time::Instant`).
  2. Send published packets across a `tokio::sync::watch::channel`.
  3. Keep the async Tokio loop dedicated solely to Axum HTTP/WebSocket request handling.

---

### [x] FIX-026: Stale Play State Retained When Exiting to Song Select / Menu
> **FIXED (scope corrected against tosu source)** — `fb8221b`. There is no `updatePlayScores(memory, isMenu)` in tosu; `case GameState.main` does nothing, so tosu **freezes** in the main menu and only resets in song select. Clearing on 2→0/7→0 as written would have been a NEW parity regression. Gated on a `isDefaultState` latch, not a prev/next table — `2→0→5` defeats the table. `play.accuracy` restores to 100, as `GameplayState.init` does.

- **File:** `src/session.rs` (`SoloSession::poll`)
- **Issue:** `clear_play_state_for_new_map` is only called if `checksum_changed` is true. If a player fails, retries, or quits back to song select on the *same beatmap*, `play.score`, `play.combo`, `play.accuracy`, and `play.hits` retain values from the old run. Overlays sitting in song select display the old play's score and combo.
- **Implement:** On state transitions from `2` (play) or `7` (results) back to `0` (menu) or `5` (song select), clear the live `play.*` fields to their default idle values.
- **Test:** Simulate state transitioning from 2 -> 5 and verify `packet.play.score == 0` and `packet.play.combo.current == 0`.

---

### [x] FIX-011: Arbitrary File Read / Path Traversal in handle_beatmap_background
> **FIXED** — `8f3d1ae`. Candidates are canonicalized and required to start with the canonical songs root; the canonical path is served. Canonicalization, not string matching, so out-and-back is accepted and out is rejected. **Deliberate behaviour change:** now fails closed when `folders.songs` is empty, which broke one existing test (updated).

- **File:** `src/server.rs` (`handle_beatmap_background`, lines 219–227)
- **Issue:** While `/overlays/` serving has traversal guards in `overlays.rs`, `/files/beatmap/background` does:
  ```rust
  if !background.trim().is_empty() {
      candidates.push(std::path::PathBuf::from(background.trim()));
  }
  ```
  `background` is read from memory and passed directly to `overlays::read_file(&path)` without verifying that it resides within `songs_folder`.
- **Implement:** Canonicalize the resolved candidate path and explicitly assert `canonical_path.starts_with(&canonical_songs_dir)`. Reject and return 404 if it escapes.
- **Test:** Pass an absolute path outside the Songs directory and assert it returns 404.

---

### [x] FIX-027: Nightcore (NC) Ignored in calculate_unstable_rate
> **FIXED** — `8deca70`. `DT || NC`. osu! stable normally sets DT alongside NC, so this is a missed site rather than a user-visible fix — the same DT-or-NC test already existed in three other files.

- **File:** `src/client.rs` (lines 559–563)
- **Issue:** `calculate_unstable_rate` only checks `if mods & 64 != 0`. Nightcore is mod bit `512` (`1 << 9`). If NC is active without the legacy DT bit set, the unstable rate is not divided by 1.5.
- **Implement:** Update check to `if (mods & 64) != 0 || (mods & 512) != 0 { rate / 1.5 }`.
- **Test:** Assert UR calculation with `mods = 512` divides variance by 1.5.

---

### [x] FIX-028: Thread Panic / Lock Poisoning in pp.rs Background Task
> **FIXED** — `9e62408`. The real defect was the `.ok()` on `Builder::spawn`: a failed spawn returns the closure, so the hand-written key removal never ran and that map was pinned to the synchronous fallback for the life of the process. Replaced by an RAII guard. **The proposed test is unwritable** — `panic = "abort"` means there is no unwind to catch; the two tests written instead pin the guard's contract directly, including the never-started-worker case.

- **File:** `src/pp.rs` (`get_or_compute_gradual_chunks`, lines 167–183)
- **Issue:** `in_progress()` stores `(map_id, mods)` keys during background calculation. If thread spawning fails or `compute_chunks` panics on a malformed map, the key is never removed from `in_progress()`, permanently disabling gradual PP computation for that map.
- **Implement:** If `std::thread::Builder::spawn` returns `Err`, immediately remove the key. Wrap `compute_chunks` inside the worker thread with an RAII guard (or `std::panic::catch_unwind`) so the key is removed on drop/panic.
- **Test:** Trigger a panic in `compute_chunks` and assert `in_progress().contains(&key)` is `false` afterwards.

---

### [~] FIX-029: Populate Strain and Hit Window Breakdown for Taiko, Catch, and Mania
> **PARTIAL — taiko only** — `3e2773d`. Taiko now fills stamina/rhythm/color/reading and the great/ok hit windows. **Catch and mania cannot be written against rosu-pp-gemini 5.0.1**: `CatchDifficultyAttributes` is `{stars, preempt, n_fruits, n_droplets, n_tiny_droplets, is_convert}` and `ManiaDifficultyAttributes` is `{stars, n_objects, n_hold_notes, max_combo, is_convert}` — neither has one skill value or hit window. Zeros pinned by test so osu!std numbers cannot leak in. Needs a newer difficulty calculator.

- **File:** `src/beatmap.rs` (`populate_beatmap_statistics_with_diff`, lines 504–517)
- **Issue:** The code only matches `DifficultyAttributes::Osu`. For Taiko, Catch, and Mania, `snapshot.stats.hit_window` (`meh`, `ok`, `great`, `miss`) and star breakdown values remain `0.0`.
- **Implement:** Add match arms for `DifficultyAttributes::Taiko`, `DifficultyAttributes::Catch`, and `DifficultyAttributes::Mania`. Extract great/ok hit windows and relevant star ratings provided by `rosu-pp`.
- **Test:** Run diff calculation on a Taiko and Mania map and assert `snapshot.stats.hit_window.great > 0.0`.

---

### [x] FIX-007: Poisoned-Mutex expect() Panics Process
> **FIXED** — `3102edc`. The audit was RIGHT and an earlier commit in this branch was wrong: `client.rs:502`/`509` really are mutex `.expect()`s, not `?`-propagating `Result` returns. An earlier pass dismissed them without verifying, and `audit.md` now carries the correction. All 7 mutex sites use `unwrap_or_else(PoisonError::into_inner)`.

- **Files:** `src/client.rs` (lines 182, 212, 335)
- **Issue:** `results.lock().expect(...)` causes an unrecoverable crash if a thread was interrupted or panicked.
- **Implement:** Replace `.expect(...)` on cache and result mutexes with `.unwrap_or_else(|poisoned| poisoned.into_inner())`.

---

## P2 — Polish, Standards Compliance & Code Health

### [x] FIX-030: Unencoded Spaces in HTTP Redirect Headers & Overlay URLs
> **FIXED** — `8f3d1ae`. `slug_segment` deleted (it *decoded* a directory name read off disk that was never encoded, so `100%20Pure` became `100 Pure`); `percent_encode` added, escaping everything outside RFC 3986 unreserved including `%`. Fixes a live bug: the shipped `browser_overlays` has `rtosu Example` and `rtosu Tourney`, both emitted as raw-space `Location` headers.

- **Files:** `src/server.rs` (line 278), `src/overlays.rs` (`slug_segment`, line 150)
- **Issue:** `handle_overlay_redirect` issues `Redirect::permanent` with raw spaces: `/overlays/rtosu Example/`. RFC 7230 / 9110 forbids unencoded spaces in `Location` headers. Also, `Overlay::url()` calls `percent_decode` instead of percent-encoding the slug, creating unescaped spaces in HTML attributes.
- **Implement:** Percent-encode the slug in `Redirect::permanent` and `Overlay::url()`.

---

### [x] FIX-004: Remove Nonexistent AudioLength Parsing & Fix Hit-Object Counter Desync
> **FIXED (first claim rejected)** — `698fdc2`. Claim 1 is wrong: the `unwrap_or(0)` fallback matches no type bit, so a malformed line was already excluded from the `objects.total` sum. The real defect is that it widened `first_object`/`last_object`, which is fixed. `AudioLength` removed — not a `[General]` option in v14, so it could never fire.

- **File:** `src/beatmap.rs` (`populate_beatmap_file_metadata`)
- **Issue:** 
  1. `values[3].parse::<u32>().unwrap_or(0)` yields `kind = 0` on bad lines, contributing to total objects without incrementing circles/sliders/spinners.
  2. `AudioLength:` does not exist in `.osu` v14 format files; searching for it is dead code.
- **Implement:** Continue on parse failure for hit objects. Remove the `AudioLength` check from the file metadata parser.

---

### [x] FIX-005: Make BeatmapStats.pp Serialization Explicit
> **REJECTED** — tosu v2's `beatmap.stats` has **no** `pp` field. PP lives at `play.pp.{current,fc,maxAchievedThisPlay}`. `#[serde(skip)]` is correct, and a test now asserts the serialized object stays exactly the four documented fields.

- **File:** `src/beatmap.rs` (`BeatmapStats`)
- **Issue:** `#[serde(skip)]` on `pub pp: BeatmapPpStats` omits `pp` from `beatmap.stats`.
- **Implement:** Either serialize `pp` as `{ "ss": f32, "fc": f32 }` under `beatmap.stats` to match tosu v2 drop-in expectations, or add a clear comment and documentation explaining that `play.pp.fc` is the canonical v2 location.

---

### [x] FIX-010: Configurable/Bounded Buffer in read_dotnet_string
> **REJECTED** — `read_dotnet_string` reads the .NET length header and returns an error on over-length rather than truncating, so the described failure mode — a silently torn string — is not reachable. A larger ceiling is a reasonable memory question but a different item.

- **File:** `src/process.rs` (`read_dotnet_string`)
- **Issue:** Hardcoded limit of 2048 characters causes heavily-tagged beatmaps or long chat messages to error or silently truncate.
- **Implement:** Read the 32-bit length header, clamp to a safe ceiling (e.g. 64 KB), and allocate accordingly with a debug log on truncation.

---

### [x] FIX-014: Replace Raw Mod Magic Numbers with Named Constants
> **FIXED** — `8deca70`. A `mod_bits` constant module; every single-bit literal in `client.rs`/`beatmap.rs`/`session.rs`/`v2.rs` replaced, including the two most misreadable (the ScoreV2 strip and the ScoreV2 set). `mod_acronyms` deliberately still chunks, per FIX-001. Parity pinned: `format_mods(536873225) == "HDHTNFATv2"` and the tosu checksum unchanged.

- **Files:** `src/beatmap.rs`, `src/client.rs`, `src/session.rs`, `src/v2.rs`
- **Issue:** Literals like `64`, `512`, `256`, `16`, `2`, `1 << 29` are scattered throughout the codebase.
- **Implement:** Define a `mod_bits` constant module (`DT = 1 << 6`, `NC = 1 << 9`, `HT = 1 << 8`, `HR = 1 << 4`, `EZ = 1 << 1`, `SCORE_V2 = 1 << 29`, etc.) and replace all raw bitwise literals.

---

### [x] FIX-031: Eliminate Heavy chrono Date Allocations on Every Trace Log
> **REJECTED** — `write_buf` formats the date only when a record is actually emitted, not per tick. The allocation is real but bounded by the log rate, on a path already behind a lock and doing disk I/O.

- **File:** `src/logging.rs` (`write_buf`)
- **Issue:** `Local::now().format("%Y-%m-%d").to_string()` allocates a new heap `String` on every single log write inside a locked mutex.
- **Implement:** Cache the current date or check against the Unix epoch midnight threshold. Only re-format the date string when the day boundary is crossed.
