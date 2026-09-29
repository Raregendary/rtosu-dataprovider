//! The StreamCompanion payload, served at `/json/sc`.
//!
//! tosu exposes this shape so overlays written against **StreamCompanion** -- a
//! separate .NET app that is not a memory reader -- can point at a memory reader
//! instead. It is built at
//! `tosu-sourcecode/packages/tosu/src/api/utils/buildResultSC.ts` and served by
//! `router/scApi.ts:5`.
//!
//! It looks nothing like v1 or v2: **136 top-level keys, ~11 KB, almost entirely
//! flat.** Bare names, no `menu`/`gameplay`/`resultsScreen` subtrees, mixed key
//! casing, and per-field units that differ from every other payload. It is built
//! here as a pure reshape of the v2 packet, like [`crate::v1`], so there is one
//! reader, one cache and one source of truth.
//!
//! # Read the assembler, not the schema
//!
//! The wire order is the object-literal order of `buildResultSC.ts`, and it is
//! **not** `api/types/sc.ts`'s declaration order. `banchoIsConnected` precedes
//! `banchoId`, `skin` precedes `skinPath`, and the four `99_9`/mania keys are
//! emitted last at `:284-287`. The declaration order in this file is the
//! assembler's, transcribed key by key, and a test pins the whole list.
//!
//! # Five leaves are JSON wrapped in a string
//!
//! `keyOverlay`, `leaderBoardMainPlayer`, `leaderBoardPlayers`,
//! `songSelectionScores` and `songSelectionMainPlayerScore` are **strings whose
//! contents are JSON**, because tosu passes them through `JSON.stringify` before
//! putting them in the object (`buildResultSC.ts:169, 204, 224, 278, 279`). A
//! consumer has to parse them a second time. They are typed [`String`] here and
//! the nested shapes are built by a typed struct, so the inner key order is the
//! assembler's too and cannot drift into rtosu's own field naming.
//!
//! # Six leaves are the numeric `GradeEnum`
//!
//! `grade` and `maxGrade` are numbers, not the grade strings every other payload
//! uses: `buildResultSC.ts:193-194` indexes the `GradeEnum` enum
//! (`common/enums/osu.ts:1-11`) rather than a grade name. `gradeCurrent` is a
//! string in v2, so the builder maps it back through `grade_enum_index`.
//!
//! # Two things that are *not* differences, so nobody "fixes" them
//!
//! **Integral floats print with a decimal point.** `serde_json` renders `200.0`
//! where `JSON.stringify` renders `200`. Both parse to the same number and no
//! consumer can tell them apart, and every rtosu payload -- v1 and v2 included
//! -- already behaves this way, so matching it here is consistency rather than a
//! new gap. What *does* matter is [`ScStrains`]' **keys**, which are strings: a
//! key of `"0"` and a key of `"0.0"` are different keys, so those go through
//! `js_number`.
//!
//! **Exact pp parity is unreachable.** rtosu uses `rosu-pp-gemini 5.0.1` and
//! tosu uses `@tosuapp/lazer-calculator-prebuilt`
//! (`tosu-sourcecode/packages/tosu/package.json:21`): two implementations of one
//! specification. Roughly 23 of the 136 leaves are pp-bearing and all of them
//! differ by some amount. No constant is fitted anywhere in this file; see
//! `validations/audits/audit-1.0.5.md` `G-08` and `M-05`. Compare shape and type, never magnitude.
//!
//! # What is not implemented, and why
//!
//! The leaves with no osu! stable source are listed in `validations/audits/audit-1.0.5.md` `M-03`.
//! Each ships **tosu's own literal** with a comment naming the row, because a
//! plausible-looking invented number is worse than an absent key: a consumer
//! cannot tell a fabricated value from a real read. The seven fields where rtosu
//! has a *better* value than tosu match tosu per decision D4, which is the
//! documented default; each carries a comment saying so.

use std::fmt::Write as _;

use serde::Serialize;

use crate::v2::TosuV2Packet;

/// tosu's `fixDecimals` (`tosu-sourcecode/packages/tosu/src/utils/converters.ts:19-20`):
/// `parseFloat((x || 0).toFixed(amount))`.
///
/// The `|| 0` is load-bearing. JavaScript treats `NaN` as falsy, so a non-finite
/// pp or accuracy value becomes `0` rather than reaching the wire as `null`, and
/// [`crate::beatmap::round_value`] on its own would propagate it.
fn fix_decimals(value: f32, decimals: u32) -> f32 {
    if value.is_finite() {
        crate::beatmap::round_value(value, decimals)
    } else {
        0.0
    }
}

/// Render a number the way JavaScript's `String(n)` would.
///
/// Needed because two SC leaves are strings built from numbers: `mBpm`
/// (`buildResultSC.ts:105-108`) and the `mapStrains` keys. Rust's `{}` for a
/// float always keeps a `.0` (`200` would print `200.0`), and JavaScript prints
/// the shortest form that round-trips, dropping a trailing `.0` on an integral
/// value. Non-integral values fall through to Rust's shortest round-trip
/// formatting, which agrees with JavaScript for ordinary magnitudes.
fn js_number(value: f64) -> String {
    if !value.is_finite() {
        // `String(NaN)` is `"NaN"` and `String(Infinity)` is `"Infinity"`, but
        // neither can reach a key or a BPM string from this data, so the honest
        // fallback is the integral form rather than a panic.
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        let mut out = String::new();
        let _ = write!(out, "{}", value as i64);
        return out;
    }
    let mut out = String::new();
    let _ = write!(out, "{value}");
    out
}

/// tosu's `formatMilliseconds` (`packages/common/utils/manipulation.ts:4-17`).
///
/// The hours, minutes and seconds are zero-padded to two digits; **the
/// milliseconds are not.** `formatMilliseconds(5005)` is `"00:00:05.5"`, not
/// `"00:00:05.005"`. Reproducing the unpadded field is the point, so this is
/// deliberately not `{:03}`.
///
/// A negative input is also reproduced rather than clamped: tosu passes
/// `timings.full - playTime` to this, which goes negative once the playhead is
/// past the last object, and `Math.floor` plus JavaScript's sign-preserving
/// remainder then produce a negative millisecond field. [`f64::floor`] matches
/// `Math.floor` and `%` matches JavaScript's `%` for the sign of the dividend.
fn format_milliseconds(ms: f64) -> String {
    if !ms.is_finite() {
        return "00:00:00.0".to_string();
    }
    let hours = (ms / 3_600_000.0).floor();
    let minutes = ((ms % 3_600_000.0) / 60_000.0).floor();
    let seconds = ((ms % 60_000.0) / 1000.0).floor();
    let milliseconds = ms % 1000.0;
    // JavaScript's `%` yields `-0` when the result is zero and the dividend is
    // negative -- `-1000 % 1000` is `-0` -- and `String(-0)` is `"0"`. Rust
    // formats `-0.0` as `"-0"`, so the negative zero is folded to positive here.
    // Verified in `validations/js_semantics.mjs`.
    let milliseconds = if milliseconds == 0.0 {
        0.0
    } else {
        milliseconds
    };
    format!("{hours:02.0}:{minutes:02.0}:{seconds:02.0}.{milliseconds:.0}")
}

/// `GradeEnum` from `packages/common/enums/osu.ts:1-11`, which is a **numeric**
/// enum: `XH, X, SH, S, A, B, C, D, None` = 0..8.
///
/// v2 carries the grade as a name (`"D"`), so the index is recovered from the
/// name. An empty name is [`GRADE_NONE`], which is what `GradeEnum` yields for
/// a ruleset that produces no grade -- `calculate_tosu_grade` returns `""` for
/// anything above mania (`src/client.rs:1033`), and indexing the enum with `""`
/// is `undefined` in JavaScript, so tosu would serialise nothing at all there.
/// Emitting `None` is the closest defined value; the alternative is an absent
/// key, which a consumer cannot distinguish from a schema difference.
const GRADE_NONE: i32 = 8;

fn grade_enum_index(grade: &str) -> i32 {
    match grade {
        "XH" => 0,
        "X" => 1,
        "SH" => 2,
        "S" => 3,
        "A" => 4,
        "B" => 5,
        "C" => 6,
        "D" => 7,
        _ => GRADE_NONE,
    }
}

/// `StreamCompanionStatus` from `api/utils/scStatus.ts:3-10`.
///
/// `toStreamCompanionStatus(status, isWatchingReplay)` maps osu!'s `GameState`
/// onto SC's six values. `Null` (0) is never returned: every osu! state that is
/// not one of the named arms falls to `Listening`. The `isWatchingReplay` input
/// is not readable from osu! stable memory (`tosu-sourcecode/packages/tosu/src/states/global.ts:9`),
/// so rtosu always passes `false` and a replay reports as `Playing`. That is
/// `M-06`'s first item, and it is a real state rtosu cannot distinguish, not a
/// default.
fn stream_companion_status(status: i32) -> i32 {
    match status {
        2 => 2,                 // play -> Playing (or Watching, which rtosu cannot see)
        1 | 4 => 16,            // edit, selectEdit -> Editing
        7 | 14 | 17 | 18 => 32, // resultScreen, rankingVs/TagCoop/Team -> ResultsScreen
        _ => 1,                 // everything else -> Listening
    }
}

/// The strain series as SC sends it: a JSON object keyed by the x-axis time.
///
/// `buildResultSC.ts:125-130` builds it with `Object.fromEntries`, so it is an
/// object, not an array, and the keys are x-axis values rendered with JavaScript
/// number-to-string rules. Two consequences have to be reproduced:
///
/// 1. **The key text.** `{"0": â€¦, "400": â€¦}`, not `{"0.0": â€¦}`. See
///    `js_number`.
/// 2. **The key order.** `JSON.stringify` hoists array-index keys ahead of every
///    other key and sorts them ascending
///    (ECMA-262 `OrdinaryOwnPropertyKeys`), so integral times come out ascending
///    regardless of the order they were inserted in, and a non-integral time
///    keeps its insertion position after them. rtosu's x-axis is already
///    ascending, so the sort is a no-op for the integral case -- but it is the
///    documented contract, and a fractional x-axis would otherwise emit a
///    different order than tosu.
///
/// Values are clamped: `value <= 0 ? 0 : value` (`buildResultSC.ts:128`). The
/// left padding tosu inserts is `-100`/`-50` (`beatmap.ts:693-697`), so without
/// the clamp every padded sample would be negative.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScStrains {
    pub points: Vec<(f64, f64)>,
}

impl Serialize for ScStrains {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        // Partitioned exactly as `OrdinaryOwnPropertyKeys` orders them: integer
        // keys ascending first, then the rest in insertion order.
        let mut integral: Vec<(i64, f64)> = Vec::new();
        let mut fractional: Vec<(String, f64)> = Vec::new();
        for (time, raw) in &self.points {
            let value = if *raw <= 0.0 { 0.0 } else { *raw };
            if !time.is_finite() {
                continue;
            }
            if time.fract() == 0.0 && time.abs() < 4_503_599_627_370_496.0 {
                integral.push((*time as i64, value));
            } else {
                fractional.push((js_number(*time), value));
            }
        }
        integral.sort_by_key(|(time, _)| *time);

        let mut map = serializer.serialize_map(Some(integral.len() + fractional.len()))?;
        for (time, value) in integral {
            map.serialize_entry(&time.to_string(), &value)?;
        }
        for (time, value) in fractional {
            map.serialize_entry(&time, &value)?;
        }
        map.end()
    }
}

/// The inner shape of the `keyOverlay` string (`buildResultSC.ts:169-179`).
///
/// `Enabled` is tosu's `config.enableKeyOverlay`, not a value read from the
/// game -- it is a tosu setting and has no osu! counterpart, so it stays `true`
/// to match the live capture. The eight button values come from the one
/// key-overlay read (`client::read_key_overlay`, a port of
/// `memory/stable.ts:644-728`).
///
/// The `.at(0..3)` indexing with `?? false` / `?? 0` fallbacks is reproduced
/// implicitly: four buttons are always emitted, even for taiko's three-element
/// array, so `m2` is `{isPressed:false,count:0}` there too.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScKeyOverlay {
    #[serde(rename = "Enabled")]
    pub enabled: bool,
    #[serde(rename = "K1Pressed")]
    pub k1_pressed: bool,
    #[serde(rename = "K1Count")]
    pub k1_count: i32,
    #[serde(rename = "K2Pressed")]
    pub k2_pressed: bool,
    #[serde(rename = "K2Count")]
    pub k2_count: i32,
    #[serde(rename = "M1Pressed")]
    pub m1_pressed: bool,
    #[serde(rename = "M1Count")]
    pub m1_count: i32,
    #[serde(rename = "M2Pressed")]
    pub m2_pressed: bool,
    #[serde(rename = "M2Count")]
    pub m2_count: i32,
}

impl ScKeyOverlay {
    /// The same read, renamed into SC's `PascalCase` field names.
    ///
    /// SC flattens the four buttons into eight scalar keys rather than nesting
    /// four objects, so this is a reshape rather than a copy. `Enabled` is
    /// *not* derived from the read: it is tosu's own config flag.
    fn from_read(overlay: &crate::v2::KeyOverlay, enabled: bool) -> Self {
        Self {
            enabled,
            k1_pressed: overlay.k1.is_pressed,
            k1_count: overlay.k1.count,
            k2_pressed: overlay.k2.is_pressed,
            k2_count: overlay.k2.count,
            m1_pressed: overlay.m1.is_pressed,
            m1_count: overlay.m1.count,
            m2_pressed: overlay.m2.is_pressed,
            m2_count: overlay.m2.count,
        }
    }
}

/// One entry of the `leaderBoardPlayers` string (`buildResultSC.ts:216-242`).
/// **Twelve keys** -- the shape without the visibility flag.
///
/// rtosu has no scoreboard read at all (`validations/audits/audit-1.0.5.md` `I-10` / `L-09` /
/// `M-03` items 5 and 6), and the live capture showed tosu serving `[]`, so this
/// type exists to type the element and is never populated. It is not a
/// placeholder for [`ScLeaderboardMainPlayer`]: the two shapes differ by one key,
/// and conflating them is how the missing key below went unnoticed.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScLeaderboardPlayer {
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Score")]
    pub score: i32,
    #[serde(rename = "Combo")]
    pub combo: i32,
    #[serde(rename = "MaxCombo")]
    pub max_combo: i32,
    #[serde(rename = "Mods")]
    pub mods: ScLeaderboardMods,
    #[serde(rename = "Hit300")]
    pub hit_300: i32,
    #[serde(rename = "Hit100")]
    pub hit_100: i32,
    #[serde(rename = "Hit50")]
    pub hit_50: i32,
    #[serde(rename = "HitMiss")]
    pub hit_miss: i32,
    #[serde(rename = "Team")]
    pub team: i32,
    #[serde(rename = "Position")]
    pub position: i32,
    #[serde(rename = "IsPassing")]
    pub is_passing: bool,
}

/// The `leaderBoardMainPlayer` string (`buildResultSC.ts:203-222`).
///
/// **Thirteen keys**, and the first is `IsLeaderboardVisible` -- which the
/// `leaderBoardPlayers` entries do **not** carry. The live diff is what caught
/// this: the two look interchangeable from the source, and a shared struct makes
/// the shorter shape wrong in one direction or the other.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScLeaderboardMainPlayer {
    /// `gameplay.isLeaderboardVisible` (`states/gameplay.ts:24, 135`). There is
    /// no rtosu read for it; a live tosu response reported `false`, which is also
    /// what a client with no scoreboard loaded serves.
    #[serde(rename = "IsLeaderboardVisible")]
    pub is_leaderboard_visible: bool,
    #[serde(flatten)]
    pub player: ScLeaderboardPlayer,
}

/// The `Mods` sub-object of a leaderboard entry.
///
/// `ModsXor1` and `ModsXor2` are tosu's own hardcoded `-1` constants
/// (`buildResultSC.ts:211-212` for the main player, `:231-232` for the list
/// entries) -- they are not computed from anything, and `Value` carries the real
/// mod bitfield. So the constants are the `Default`, and `value` is the only field
/// a builder would vary.
///
/// The struct was originally built through a `new(value)` constructor. Nothing
/// called it: the leaderboard types are never populated (see the type docs above),
/// so the only construction sites were the two `Default`s, and the constructor was
/// dead by construction. The fields are public and the struct derives `Default`,
/// so populating it later needs no constructor.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScLeaderboardMods {
    #[serde(rename = "ModsXor1")]
    pub mods_xor_1: i32,
    #[serde(rename = "ModsXor2")]
    pub mods_xor_2: i32,
    #[serde(rename = "Value")]
    pub value: u32,
}

impl Default for ScLeaderboardMods {
    fn default() -> Self {
        Self {
            mods_xor_1: -1,
            mods_xor_2: -1,
            value: 0,
        }
    }
}

/// One `mapBreaks` entry (`buildResultSC.ts:131-135`).
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScBreak {
    #[serde(rename = "startTime")]
    pub start_time: i32,
    #[serde(rename = "endTime")]
    pub end_time: i32,
    #[serde(rename = "hasEffect")]
    pub has_effect: bool,
}

/// One `mapTimingPoints` entry (`buildResultSC.ts:138-141`).
///
/// `bpm` is declared optional in `sc.ts:85` and **never emitted** --
/// `buildResultSC.ts:138-141` writes only `startTime` and `beatLength`, so the
/// entry has two keys on the wire against three in the schema. Reproducing the
/// two-key form is the point.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScTimingPoint {
    #[serde(rename = "startTime")]
    pub start_time: i32,
    #[serde(rename = "beatLength")]
    pub beat_length: f64,
}

/// The StreamCompanion payload: 136 flat keys.
///
/// Field order **is** the wire order, and it is
/// `buildResultSC.ts`'s object-literal order rather than `sc.ts`'s declaration
/// order. Every field carries an explicit `rename` because the casing is mixed
/// (`c300`, `osu_90PP`, `mStars`, `maxCombo`) and no single `rename_all` rule
/// produces it.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ScPayload {
    // --- presence and bancho (buildResultSC.ts:54-63) ---
    /// Constant `1`. tosu only reaches this object when an instance is attached,
    /// so the field is never `0`.
    #[serde(rename = "osuIsRunning")]
    pub osu_is_running: i32,
    /// `M-03` item 1 -- **STUB, tosu's literal.** tosu reads
    /// `global.chatStatus` from osu! stable memory (`states/global.ts:14`) and
    /// rtosu does not. A live tosu response reported `0`.
    #[serde(rename = "chatIsEnabled")]
    pub chat_is_enabled: i32,
    /// `M-03` item 2 -- **STUB, tosu's literal.** Same walk as `chatStatus`:
    /// `global.showInterface` (`global.ts:12` <- `memory/stable.ts:814-821`).
    /// A live tosu response reported `0`. The identical read would also close
    /// v1's `settings.showInterface` and v2's `game.interfaceVisible`; see the
    /// open decision in `validations/audits/audit-1.0.5.md` L-03 before spending it twice.
    #[serde(rename = "ingameInterfaceIsEnabled")]
    pub ingame_interface_is_enabled: i32,
    /// **tosu quirk, reproduced.** `buildResultSC.ts:57` is a hardcoded `0` with
    /// a `// TODO`, even though rtosu computes the real value as
    /// `beatmap.is_break`. Correcting it would be a divergence, not a fix.
    #[serde(rename = "isBreakTime")]
    pub is_break_time: i32,
    #[serde(rename = "banchoIsConnected")]
    pub bancho_is_connected: i32,
    #[serde(rename = "banchoId")]
    pub bancho_id: i32,
    #[serde(rename = "banchoUsername")]
    pub bancho_username: String,
    /// `user.rawLoginStatus`, the number -- not the name v2 pairs with it.
    #[serde(rename = "banchoStatus")]
    pub bancho_status: i32,
    /// `CountryCodes[user.countryCode]?.toUpperCase() || ''`
    /// (`buildResultSC.ts:63`). v2's `profile.countryCode.name` is already
    /// uppercased (`src/reader.rs:461`), so no second uppercase is needed.
    /// Inherits the `A-01` country-table bug if it is ever unfixed; the value is
    /// whatever v2 reports, so the two payloads cannot disagree.
    #[serde(rename = "banchoCountry")]
    pub bancho_country: String,

    // --- metadata (buildResultSC.ts:65-77) ---
    #[serde(rename = "artistRoman")]
    pub artist_roman: String,
    #[serde(rename = "artistUnicode")]
    pub artist_unicode: String,
    #[serde(rename = "titleRoman")]
    pub title_roman: String,
    #[serde(rename = "titleUnicode")]
    pub title_unicode: String,
    #[serde(rename = "mapArtistTitle")]
    pub map_artist_title: String,
    #[serde(rename = "mapArtistTitleUnicode")]
    pub map_artist_title_unicode: String,
    #[serde(rename = "diffName")]
    pub diff_name: String,
    /// `` `[${menu.difficulty}]` `` -- brackets added by tosu.
    #[serde(rename = "mapDiff")]
    pub map_diff: String,
    #[serde(rename = "creator")]
    pub creator: String,

    // --- stars and difficulty (buildResultSC.ts:79-93) ---
    /// `currAttributes.stars`, the live partial -- not `stars.total`.
    #[serde(rename = "liveStarRating")]
    pub live_star_rating: f32,
    /// `calculatedMapAttributes.fullStars`.
    #[serde(rename = "mStars")]
    pub m_stars: f32,
    /// `ar`, i.e. the raw `.osu` value, **not** `mAR`.
    #[serde(rename = "ar")]
    pub ar: f32,
    /// `arConverted`, i.e. post-mods.
    #[serde(rename = "mAR")]
    pub m_ar: f32,
    #[serde(rename = "od")]
    pub od: f32,
    #[serde(rename = "mOD")]
    pub m_od: f32,
    #[serde(rename = "cs")]
    pub cs: f32,
    #[serde(rename = "mCS")]
    pub m_cs: f32,
    #[serde(rename = "hp")]
    pub hp: f32,
    #[serde(rename = "mHP")]
    pub m_hp: f32,

    // --- bpm (buildResultSC.ts:94-108) ---
    #[serde(rename = "currentBpm")]
    pub current_bpm: f32,
    #[serde(rename = "bpm")]
    pub bpm: f32,
    #[serde(rename = "mainBpm")]
    pub main_bpm: f32,
    #[serde(rename = "maxBpm")]
    pub max_bpm: f32,
    #[serde(rename = "minBpm")]
    pub min_bpm: f32,
    /// **tosu quirk, reproduced.** The whole `m*` family duplicates the plain one
    /// verbatim (`buildResultSC.ts:100-103` are copies of `:95-98`), so the `m`
    /// prefix is a lie here. Correcting it would break parity.
    #[serde(rename = "mMainBpm")]
    pub m_main_bpm: f32,
    #[serde(rename = "mMaxBpm")]
    pub m_max_bpm: f32,
    #[serde(rename = "mMinBpm")]
    pub m_min_bpm: f32,
    /// A **string**: `minBPM === maxBPM ? maxBPM.toString() :` a rounded
    /// `"min-max (common)"` range (`buildResultSC.ts:105-108`). See
    /// `js_number`.
    #[serde(rename = "mBpm")]
    pub m_bpm: String,

    // --- identity (buildResultSC.ts:110-113) ---
    #[serde(rename = "md5")]
    pub md5: String,
    /// `Rulesets[currentMode]`, the ruleset **name**.
    #[serde(rename = "gameMode")]
    pub game_mode: String,
    #[serde(rename = "mode")]
    pub mode: i32,

    // --- timing (buildResultSC.ts:115-121) ---
    /// `global.playTime / 1000` -- seconds, and the **only** float in this block
    /// that is not passed through `fixDecimals`. A live response reported
    /// `5.105` for a playhead of 5105 ms.
    #[serde(rename = "time")]
    pub time: f64,
    /// `beatmapPP.previewtime`, from `[General] PreviewTime`. A missing line is
    /// osu!lazer's `-1`, so an unresolved read reports `-1` rather than `0`.
    #[serde(rename = "previewtime")]
    pub preview_time: i32,
    /// `timings.full`, i.e. the last object's time, not the audio length.
    #[serde(rename = "totaltime")]
    pub total_time: i32,
    /// `formatMilliseconds(timings.full - playTime)`. The milliseconds are
    /// **unpadded** -- see `format_milliseconds`.
    #[serde(rename = "timeLeft")]
    pub time_left: String,
    #[serde(rename = "drainingtime")]
    pub draining_time: i32,
    /// `menu.mp3Length`, the audio-track length, **not** the map's last object.
    #[serde(rename = "totalAudioTime")]
    pub total_audio_time: i32,
    #[serde(rename = "firstHitObjectTime")]
    pub first_hit_object_time: i32,

    // --- ids and map data (buildResultSC.ts:123-142) ---
    #[serde(rename = "mapid")]
    pub map_id: i32,
    #[serde(rename = "mapsetid")]
    pub mapset_id: i32,
    /// An object keyed by x-axis time, not an array. See [`ScStrains`].
    #[serde(rename = "mapStrains")]
    pub map_strains: ScStrains,
    #[serde(rename = "mapBreaks")]
    pub map_breaks: Vec<ScBreak>,
    /// **tosu quirk, reproduced.** Always `[]` -- `buildResultSC.ts:136` is
    /// `mapKiaiPoints: [], // TODO: add` even though tosu builds the kiai spans
    /// correctly at `states/beatmap.ts:510-523` and never sends them. rtosu reads
    /// the same `effect_points`, so the real data is one line away and tosu
    /// chose not to send it.
    #[serde(rename = "mapKiaiPoints")]
    pub map_kiai_points: Vec<ScTimingPoint>,
    /// `formatMilliseconds(global.playTime)`, unpadded milliseconds.
    #[serde(rename = "mapPosition")]
    pub map_position: String,
    /// Two keys per entry, against three in `sc.ts` -- see [`ScTimingPoint`].
    #[serde(rename = "mapTimingPoints")]
    pub map_timing_points: Vec<ScTimingPoint>,

    // --- object counts and files (buildResultSC.ts:143-156) ---
    #[serde(rename = "sliders")]
    pub sliders: i32,
    #[serde(rename = "circles")]
    pub circles: i32,
    #[serde(rename = "spinners")]
    pub spinners: i32,
    #[serde(rename = "maxCombo")]
    pub max_combo: i32,
    #[serde(rename = "mp3Name")]
    pub mp3_name: String,
    #[serde(rename = "osuFileName")]
    pub osu_file_name: String,
    #[serde(rename = "backgroundImageFileName")]
    pub background_image_file_name: String,
    /// `path.join(menu.folder, menu.filename)` -- the **`.osu` file**, joined
    /// with a backslash. Note v1's `path.full` is the *background*; the two
    /// payloads disagree and each is transcribed separately.
    #[serde(rename = "osuFileLocation")]
    pub osu_file_location: String,
    /// `path.join(menu.folder, menu.backgroundFilename)`.
    #[serde(rename = "backgroundImageLocation")]
    pub background_image_location: String,

    // --- play state (buildResultSC.ts:158-177) ---
    /// `M-03` item 3 -- **STUB, tosu's literal.** `gameplay.retries`
    /// (`states/gameplay.ts:60, 219`) is not read. A live response reported `0`.
    #[serde(rename = "retries")]
    pub retries: i32,
    #[serde(rename = "username")]
    pub username: String,
    #[serde(rename = "score")]
    pub score: i32,
    /// `gameplay.playerHP` **raw**, not the bar percentage. v2's
    /// `play.healthBar.normal` is the same number divided by two
    /// (`buildResultV2.ts:917-920`: `(playerHP / 200) * 100`), so this is that
    /// value multiplied back by two. Halving and doubling by a power of two is
    /// exact in `f64`, so this recovers the original bit for bit.
    #[serde(rename = "playerHp")]
    pub player_hp: f64,
    /// The same for the smoothed bar, v2's `play.healthBar.smooth`.
    #[serde(rename = "playerHpSmooth")]
    pub player_hp_smooth: f64,
    #[serde(rename = "combo")]
    pub combo: i32,
    #[serde(rename = "currentMaxCombo")]
    pub current_max_combo: i32,
    /// **A JSON string**, not an object -- see [`ScKeyOverlay`].
    #[serde(rename = "keyOverlay")]
    pub key_overlay: String,

    // --- hits and accuracy (buildResultSC.ts:181-191) ---
    #[serde(rename = "geki")]
    pub geki: i32,
    #[serde(rename = "c300")]
    pub c300: i32,
    #[serde(rename = "katsu")]
    pub katsu: i32,
    #[serde(rename = "c100")]
    pub c100: i32,
    #[serde(rename = "c50")]
    pub c50: i32,
    #[serde(rename = "miss")]
    pub miss: i32,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i32,
    #[serde(rename = "acc")]
    pub acc: f32,
    /// **`unstableRate * mods.rate`**, i.e. the *un*-converted value. The naming
    /// is inverted from intuition, which is what made it look unfillable; see
    /// `M-04`.
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f32,
    /// The already-clock-rate-divided value, which is v2's `play.unstableRate`.
    #[serde(rename = "convertedUnstableRate")]
    pub converted_unstable_rate: f32,

    // --- grade and hit errors (buildResultSC.ts:193-196) ---
    /// The **numeric** `GradeEnum`, not the name. See `grade_enum_index`.
    #[serde(rename = "grade")]
    pub grade: i32,
    #[serde(rename = "maxGrade")]
    pub max_grade: i32,
    #[serde(rename = "hitErrors")]
    pub hit_errors: std::sync::Arc<[i16]>,

    // --- mods (buildResultSC.ts:198-199) ---
    #[serde(rename = "mods")]
    pub mods: String,
    #[serde(rename = "modsEnum")]
    pub mods_enum: u32,

    // --- live pp (buildResultSC.ts:201-202) ---
    #[serde(rename = "ppIfMapEndsNow")]
    pub pp_if_map_ends_now: f32,
    #[serde(rename = "ppIfRestFced")]
    pub pp_if_rest_fced: f32,

    // --- leaderboard (buildResultSC.ts:204-243) ---
    /// **A JSON string** -- see [`ScLeaderboardPlayer`].
    #[serde(rename = "leaderBoardMainPlayer")]
    pub leader_board_main_player: String,
    /// **A JSON string**, and `'[]'` in the absence of a scoreboard. tosu's own
    /// live response was exactly `[]`.
    #[serde(rename = "leaderBoardPlayers")]
    pub leader_board_players: String,

    // --- status (buildResultSC.ts:244-249) ---
    #[serde(rename = "rankedStatus")]
    pub ranked_status: i32,
    /// `M-03` item 7 -- **STUB, tosu's literal.** `settings.leaderboardType`
    /// (`states/settings.ts:118`) is not read. A live response reported `0`.
    #[serde(rename = "songSelectionRankingType")]
    pub song_selection_ranking_type: i32,
    /// osu!'s own `GameState` number, not SC's remapped `status` 30 lines down.
    #[serde(rename = "rawStatus")]
    pub raw_status: i32,
    #[serde(rename = "dir")]
    pub dir: String,
    /// `` `https://osu.ppy.sh/b/${mapID}` `` -- the **map** id, not the set id.
    #[serde(rename = "dl")]
    pub dl: String,

    // --- pp tables (buildResultSC.ts:251-272) ---
    // Seven of tosu's eleven accuracies (90-100), under a plain and an `m`
    // prefix. Values are rosu-pp's, not tosu's: the two calculators are
    // different implementations of one specification and the gap is
    // structurally unreachable (validations/audits/audit-1.0.5.md G-08). No constant is fitted.
    #[serde(rename = "osu_90PP")]
    pub osu_90pp: f32,
    #[serde(rename = "osu_95PP")]
    pub osu_95pp: f32,
    #[serde(rename = "osu_96PP")]
    pub osu_96pp: f32,
    #[serde(rename = "osu_97PP")]
    pub osu_97pp: f32,
    #[serde(rename = "osu_98PP")]
    pub osu_98pp: f32,
    #[serde(rename = "osu_99PP")]
    pub osu_99pp: f32,
    /// `ppAcc[100]`. Note there is no `osu_91PP` .. `osu_94PP`: tosu's table runs
    /// 90-100 and SC surfaces seven of the eleven.
    #[serde(rename = "osu_SSPP")]
    pub osu_sspp: f32,
    /// **tosu quirk, reproduced** -- `buildResultSC.ts:258-264` is a verbatim copy
    /// of `:251-257`, so every `osu_m*PP` equals its `osu_*PP`.
    #[serde(rename = "osu_m90PP")]
    pub osu_m90pp: f32,
    #[serde(rename = "osu_m95PP")]
    pub osu_m95pp: f32,
    #[serde(rename = "osu_m96PP")]
    pub osu_m96pp: f32,
    #[serde(rename = "osu_m97PP")]
    pub osu_m97pp: f32,
    #[serde(rename = "osu_m98PP")]
    pub osu_m98pp: f32,
    #[serde(rename = "osu_m99PP")]
    pub osu_m99pp: f32,
    #[serde(rename = "osu_mSSPP")]
    pub osu_msspp: f32,
    #[serde(rename = "accPpIfMapEndsNow")]
    pub acc_pp_if_map_ends_now: f32,
    #[serde(rename = "aimPpIfMapEndsNow")]
    pub aim_pp_if_map_ends_now: f32,
    #[serde(rename = "speedPpIfMapEndsNow")]
    pub speed_pp_if_map_ends_now: f32,
    /// `ppDifficulty`. osu!standard has no difficulty skill, so this is
    /// structurally `0.0` there (validations/audits/audit-1.0.5.md `M-05`).
    #[serde(rename = "strainPpIfMapEndsNow")]
    pub strain_pp_if_map_ends_now: f32,
    /// **tosu quirk, reproduced** -- this is `currAttributes.fcPP`, the same
    /// value as `ppIfRestFced` above, and it carries upstream's own
    /// `// TODO: idk if it's correct` at `buildResultSC.ts:272`. The doubt is
    /// upstream's, not a data limit.
    #[serde(rename = "noChokePp")]
    pub no_choke_pp: f32,

    // --- skin (buildResultSC.ts:274-275) ---
    /// `global.skinFolder`. tosu emits the **same string** for both keys
    /// (`:274-275`), so `skin` and `skinPath` cannot disagree.
    #[serde(rename = "skin")]
    pub skin: String,
    #[serde(rename = "skinPath")]
    pub skin_path: String,

    // --- song selection (buildResultSC.ts:278-282) ---
    /// `M-03` item 14 -- **STUB, tosu's literal `'[]'`.** tosu never populates
    /// this from anywhere; it is a hardcoded string in the object literal.
    #[serde(rename = "songSelectionScores")]
    pub song_selection_scores: String,
    /// `M-03` item 14 -- **STUB, tosu's literal `'{}'`.**
    #[serde(rename = "songSelectionMainPlayerScore")]
    pub song_selection_main_player_score: String,
    #[serde(rename = "songSelectionTotalScores")]
    pub song_selection_total_scores: i32,
    /// SC's remapped status -- see `stream_companion_status`.
    #[serde(rename = "status")]
    pub status: i32,

    // --- the four keys tosu emits last (buildResultSC.ts:284-287) ---
    /// `M-03` item 10 -- **STUB, tosu's literal `-1`.** 99.9 % is not in tosu's
    /// `ppAcc` table (90-100) and has no SC bucket of its own. Adding a 99.9 row
    /// to rtosu's own table would be a shape change, so tosu's `-1` it is.
    #[serde(rename = "osu_99_9PP")]
    pub osu_99_9pp: i32,
    #[serde(rename = "osu_m99_9PP")]
    pub osu_m99_9pp: i32,
    /// `M-03` item 8 -- **STUB, tosu's literal `-1`.** No mania key-count pp axis
    /// exists in any calculator rtosu has. A lazer calculator would allow the
    /// sweep; the *bucket choice* is SC-side either way.
    #[serde(rename = "mania_1_000_000PP")]
    pub mania_1_000_000pp: i32,
    #[serde(rename = "mania_m1_000_000PP")]
    pub mania_m1_000_000pp: i32,
    /// `ppAcc[100]` again -- a third copy of the same number
    /// (`buildResultSC.ts:288`), alongside `osu_SSPP` and `osu_mSSPP`.
    #[serde(rename = "simulatedPp")]
    pub simulated_pp: f32,

    // --- fields rtosu has better values for (buildResultSC.ts:290-292) ---
    /// Matches tosu per D4. rtosu reads a real play count
    /// (`src/client.rs:33`) and tosu hardcodes `0` (`buildResultSC.ts:290`).
    /// Deviating needs a decision; see `validations/audits/audit-1.0.5.md` `M-06`.
    #[serde(rename = "plays")]
    pub plays: i32,
    /// Matches tosu per D4 -- `''` against rtosu's real `.osu` tags.
    #[serde(rename = "tags")]
    pub tags: String,
    /// Matches tosu per D4 -- `''` against rtosu's real `.osu` source.
    #[serde(rename = "source")]
    pub source: String,

    // --- stream/leaderboard identity (buildResultSC.ts:294-303) ---
    /// `M-03` item 11 -- **STUB, tosu's literal `-1`.** No stable-side source;
    /// this is StreamCompanion's own stream handle.
    #[serde(rename = "starsNomod")]
    pub stars_nomod: i32,
    /// `M-03`/`M-04` -- **STUB, tosu's literal `-1`.** tosu's own comment is
    /// `// TODO: we dont have that` at `buildResultSC.ts:295`. rtosu *could*
    /// derive it as `max_combo - combo` clamped at 0, which is `M-04`'s last
    /// row, but D4's default is to match tosu and the deviation is not decided.
    #[serde(rename = "comboLeft")]
    pub combo_left: i32,
    /// Matches tosu per D4: `0` where rtosu has a real RFC3339 timestamp.
    /// `M-06` flags this one as deserving an explicit decision rather than the
    /// default, because "match tosu" means shipping a wrong time. **Open.**
    #[serde(rename = "localTime")]
    pub local_time: i32,
    /// Matches tosu per D4: the literal string `'0'`, not a formatted time.
    #[serde(rename = "localTimeISO")]
    pub local_time_iso: String,
    /// `M-03` item 11 -- **STUB, tosu's literal `-1`.**
    #[serde(rename = "sl")]
    pub sl: i32,
    /// `M-03` item 11 -- **STUB, tosu's literal `-1`.**
    #[serde(rename = "sv")]
    pub sv: i32,
    /// `M-03` item 12 -- **STUB, tosu's literal `0`.** A chat-system identity,
    /// absent from stable memory.
    #[serde(rename = "threadid")]
    pub threadid: i32,
    /// `M-03` item 13 -- **STUB, tosu's literal `''`.** A debug hook tosu leaves
    /// empty.
    #[serde(rename = "test")]
    pub test: String,
}

impl ScPayload {
    /// Reshape a v2 packet into the SC wire shape.
    ///
    /// Pure, so the whole mapping is testable without osu! running, like
    /// [`crate::v1::GosuCompatibleApi::from_v2`].
    pub fn from_v2(packet: &TosuV2Packet) -> Self {
        let b = &packet.beatmap;
        let stats = &b.stats;
        let play = &packet.play;
        let profile = &packet.profile;
        let acc_table = &packet.performance.accuracy;

        // The graph is held pre-serialised (`PrecomputedGraph` wraps a `RawValue`)
        // so v2 does not re-encode it per poll. SC needs the first series against
        // the x-axis, so it is decoded here -- per *request*, not per poll, which
        // is what keeps this off the reader's hot path.
        //
        // `mapStrains` uses `strainsAll.series[0]`, the same first series v1 uses
        // for its `strains` block (`buildResultSC.ts:51, 125-130`). For a
        // non-osu!std map rtosu emits no series, so this is empty rather than
        // wrong -- see the BLOCKED note on per-mode strains in
        // `validations/audits/audit-1.0.5.md` `G-03` and `L-02`.
        let graph = packet.performance.graph.decoded();
        let strain_values = graph
            .series
            .first()
            .map(|s| s.data.as_slice())
            .unwrap_or(&[]);

        // **The playhead is `beatmap.time.live`, not `session.play_time`.**
        // tosu's v2 exposes the same number twice under two names, and the SC
        // payload wants the other one:
        //
        //   `global.playTime`  -> v2 `beatmap.time.live`  (5105 ms)
        //   `global.gameTime`  -> v2 `session.playTime`   (59,064,147 ms)
        //
        // `buildResultSC.ts:116, 120, 145` all read `global.playTime`, so the
        // whole `time` / `timeLeft` / `mapPosition` family comes from
        // `beatmap.time.live`. Using `session.play_time` reports a song position
        // 16 hours ahead of the real one, and turns `timeLeft` into a negative
        // clock. Caught by the live value diff, where the two sides disagreed on
        // exactly these three leaves while agreeing on `mapid` and `score`.
        let play_time = b.time.live as f64;
        let full_time = b.time.last_object;
        let common_bpm = stats.bpm.common;
        let min_bpm = stats.bpm.min;
        let max_bpm = stats.bpm.max;

        Self {
            osu_is_running: 1,
            chat_is_enabled: 0,
            ingame_interface_is_enabled: 0,
            is_break_time: 0,

            bancho_is_connected: i32::from(profile.user_status.number == 65793),
            bancho_id: profile.id,
            bancho_username: profile.name.clone(),
            bancho_status: profile.user_status.number,
            bancho_country: profile.country_code.name.clone(),

            artist_roman: b.artist.clone(),
            artist_unicode: b.artist_unicode.clone(),
            title_roman: b.title.clone(),
            title_unicode: b.title_unicode.clone(),
            map_artist_title: format!("{} - {}", b.artist, b.title),
            map_artist_title_unicode: format!("{} - {}", b.artist_unicode, b.title_unicode),
            diff_name: b.version.clone(),
            map_diff: format!("[{}]", b.version),
            creator: b.mapper.clone(),

            live_star_rating: fix_decimals(stats.stars.live, 2),
            m_stars: fix_decimals(stats.stars.total, 2),

            // `ar`/`od`/`cs`/`hp` are the raw `.osu` values; the `m`-prefixed pair
            // is the mods-converted one. That is the opposite of what the `m`
            // might suggest, and it is what `buildResultSC.ts:82-93` does.
            ar: fix_decimals(stats.ar.original, 2),
            m_ar: fix_decimals(stats.ar.converted, 2),
            od: fix_decimals(stats.od.original, 2),
            m_od: fix_decimals(stats.od.converted, 2),
            cs: fix_decimals(stats.cs.original, 2),
            m_cs: fix_decimals(stats.cs.converted, 2),
            hp: fix_decimals(stats.hp.original, 2),
            m_hp: fix_decimals(stats.hp.converted, 2),

            current_bpm: fix_decimals(stats.bpm.realtime, 4),
            bpm: fix_decimals(common_bpm, 4),
            main_bpm: fix_decimals(common_bpm, 4),
            max_bpm: fix_decimals(max_bpm, 4),
            min_bpm: fix_decimals(min_bpm, 4),
            m_main_bpm: fix_decimals(common_bpm, 4),
            m_max_bpm: fix_decimals(max_bpm, 4),
            m_min_bpm: fix_decimals(min_bpm, 4),
            // `minBPM === maxBPM` compares the raw values, before `fixDecimals`.
            m_bpm: if min_bpm == max_bpm {
                js_number(max_bpm as f64)
            } else {
                format!(
                    "{}-{} ({})",
                    js_number(min_bpm.round() as f64),
                    js_number(max_bpm.round() as f64),
                    js_number(common_bpm.round() as f64)
                )
            },

            md5: b.checksum.clone(),
            game_mode: crate::reader::ruleset_name(play.mode.number).to_string(),
            mode: play.mode.number,

            time: play_time / 1000.0,
            // A missing `PreviewTime` line is osu!lazer's `-1`, not `0`.
            preview_time: b.preview_time.unwrap_or(-1),
            total_time: full_time,
            time_left: format_milliseconds(full_time as f64 - play_time),
            draining_time: full_time - b.time.first_object,
            total_audio_time: b.time.mp3_length,
            first_hit_object_time: b.time.first_object,

            map_id: b.id,
            mapset_id: b.set,
            map_strains: ScStrains {
                points: graph
                    .xaxis
                    .iter()
                    .enumerate()
                    .map(|(index, time)| (*time, strain_values.get(index).copied().unwrap_or(0.0)))
                    .collect(),
            },
            map_breaks: b
                .breaks
                .iter()
                .map(|span| ScBreak {
                    start_time: span.start_time,
                    end_time: span.end_time,
                    has_effect: span.has_effect,
                })
                .collect(),
            map_kiai_points: Vec::new(),
            map_position: format_milliseconds(play_time),
            map_timing_points: b
                .timing_points
                .iter()
                .map(|point| ScTimingPoint {
                    start_time: point.time,
                    beat_length: point.beat_length,
                })
                .collect(),

            sliders: stats.objects.sliders,
            circles: stats.objects.circles,
            spinners: stats.objects.spinners,
            max_combo: stats.max_combo,

            mp3_name: b.audio_filename.clone(),
            osu_file_name: b.filename.clone(),
            background_image_file_name: b.background_filename.clone(),
            osu_file_location: crate::reader::join_path(&b.folder, &b.filename),
            background_image_location: crate::reader::join_path(&b.folder, &b.background_filename),

            retries: 0,
            username: play.player_name.clone(),
            score: play.score,
            // `playerHP` raw, so v2's `/200*100` bar is multiplied back by two.
            player_hp: play.health_bar.normal * 2.0,
            player_hp_smooth: play.health_bar.smooth * 2.0,
            combo: play.combo.current,
            current_max_combo: play.combo.max,
            key_overlay: encode_json(&ScKeyOverlay::from_read(
                &play.key_overlay,
                // tosu's `config.enableKeyOverlay`, not a value read from the
                // game. Always on, matching the live capture.
                true,
            )),

            geki: play.hits.geki,
            c300: play.hits.n300,
            katsu: play.hits.katu,
            c100: play.hits.n100,
            c50: play.hits.n50,
            miss: play.hits.n0,
            slider_breaks: play.hits.slider_breaks,
            acc: fix_decimals(play.accuracy as f32, 2),
            // The naming is inverted from intuition: rtosu's `unstable_rate` is
            // already clock-rate divided, so it is `convertedUnstableRate` and
            // the plain key is that value scaled back up (`M-04`).
            unstable_rate: fix_decimals(play.unstable_rate as f32 * play.mods.rate, 2),
            converted_unstable_rate: fix_decimals(play.unstable_rate as f32, 2),

            grade: grade_enum_index(&play.rank.current),
            max_grade: grade_enum_index(&play.rank.max_this_play),
            hit_errors: play.hit_error_array.clone(),

            mods: play.mods.name.clone(),
            mods_enum: play.mods.number,

            pp_if_map_ends_now: fix_decimals(play.pp.current, 2),
            pp_if_rest_fced: fix_decimals(play.pp.fc, 2),

            leader_board_main_player: encode_json(&ScLeaderboardMainPlayer::default()),
            leader_board_players: "[]".to_string(),

            ranked_status: b.status.number,
            song_selection_ranking_type: 0,
            raw_status: packet.state.number,
            dir: b.folder.clone(),
            dl: format!("https://osu.ppy.sh/b/{}", b.id),

            osu_90pp: fix_decimals(acc_table.n90, 2),
            osu_95pp: fix_decimals(acc_table.n95, 2),
            osu_96pp: fix_decimals(acc_table.n96, 2),
            osu_97pp: fix_decimals(acc_table.n97, 2),
            osu_98pp: fix_decimals(acc_table.n98, 2),
            osu_99pp: fix_decimals(acc_table.n99, 2),
            osu_sspp: fix_decimals(acc_table.n100, 2),
            osu_m90pp: fix_decimals(acc_table.n90, 2),
            osu_m95pp: fix_decimals(acc_table.n95, 2),
            osu_m96pp: fix_decimals(acc_table.n96, 2),
            osu_m97pp: fix_decimals(acc_table.n97, 2),
            osu_m98pp: fix_decimals(acc_table.n98, 2),
            osu_m99pp: fix_decimals(acc_table.n99, 2),
            osu_msspp: fix_decimals(acc_table.n100, 2),
            acc_pp_if_map_ends_now: fix_decimals(play.pp.detailed.current.accuracy, 2),
            aim_pp_if_map_ends_now: fix_decimals(play.pp.detailed.current.aim, 2),
            speed_pp_if_map_ends_now: fix_decimals(play.pp.detailed.current.speed, 2),
            strain_pp_if_map_ends_now: fix_decimals(play.pp.detailed.current.difficulty, 2),
            no_choke_pp: fix_decimals(play.pp.fc, 2),

            skin: packet.folders.skin.clone(),
            skin_path: packet.folders.skin.clone(),

            song_selection_scores: "[]".to_string(),
            song_selection_main_player_score: "{}".to_string(),
            song_selection_total_scores: -1,
            status: stream_companion_status(packet.state.number),

            osu_99_9pp: -1,
            osu_m99_9pp: -1,
            mania_1_000_000pp: -1,
            mania_m1_000_000pp: -1,
            simulated_pp: fix_decimals(acc_table.n100, 2),

            plays: 0,
            tags: String::new(),
            source: String::new(),

            stars_nomod: -1,
            combo_left: -1,
            local_time: 0,
            local_time_iso: "0".to_string(),
            sl: -1,
            sv: -1,
            threadid: 0,
            test: String::new(),
        }
    }
}

/// Serialize a nested shape into a string field, the way `JSON.stringify` does.
///
/// Five SC leaves are JSON wrapped in a string because tosu stringifies them
/// before putting them in the object. Falling back to `"null"` keeps the outer
/// payload valid JSON if the inner encode ever fails; the inner types are plain
/// structs, so that is unreachable, and a wrong value would be better than a
/// missing key for a consumer that parses the string.
fn encode_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beatmap::BeatmapMode;
    use crate::pp::PpBreakdown;
    use crate::v2::OsuStatusState;

    /// The wire order, transcribed key by key from a live tosu `/json/sc`
    /// response and checked against `buildResultSC.ts`'s object-literal order.
    ///
    /// It is **not** `api/types/sc.ts`'s declaration order: that file lists
    /// `banchoUsername` before `banchoId`, `skinPath` before `skin`, and the
    /// `99_9`/mania keys in the middle. Serde emits declaration order, so this
    /// list *is* the struct's field order and a drift in either shows up here.
    const EXPECTED_KEYS: &[&str] = &[
        "osuIsRunning",
        "chatIsEnabled",
        "ingameInterfaceIsEnabled",
        "isBreakTime",
        "banchoIsConnected",
        "banchoId",
        "banchoUsername",
        "banchoStatus",
        "banchoCountry",
        "artistRoman",
        "artistUnicode",
        "titleRoman",
        "titleUnicode",
        "mapArtistTitle",
        "mapArtistTitleUnicode",
        "diffName",
        "mapDiff",
        "creator",
        "liveStarRating",
        "mStars",
        "ar",
        "mAR",
        "od",
        "mOD",
        "cs",
        "mCS",
        "hp",
        "mHP",
        "currentBpm",
        "bpm",
        "mainBpm",
        "maxBpm",
        "minBpm",
        "mMainBpm",
        "mMaxBpm",
        "mMinBpm",
        "mBpm",
        "md5",
        "gameMode",
        "mode",
        "time",
        "previewtime",
        "totaltime",
        "timeLeft",
        "drainingtime",
        "totalAudioTime",
        "firstHitObjectTime",
        "mapid",
        "mapsetid",
        "mapStrains",
        "mapBreaks",
        "mapKiaiPoints",
        "mapPosition",
        "mapTimingPoints",
        "sliders",
        "circles",
        "spinners",
        "maxCombo",
        "mp3Name",
        "osuFileName",
        "backgroundImageFileName",
        "osuFileLocation",
        "backgroundImageLocation",
        "retries",
        "username",
        "score",
        "playerHp",
        "playerHpSmooth",
        "combo",
        "currentMaxCombo",
        "keyOverlay",
        "geki",
        "c300",
        "katsu",
        "c100",
        "c50",
        "miss",
        "sliderBreaks",
        "acc",
        "unstableRate",
        "convertedUnstableRate",
        "grade",
        "maxGrade",
        "hitErrors",
        "mods",
        "modsEnum",
        "ppIfMapEndsNow",
        "ppIfRestFced",
        "leaderBoardMainPlayer",
        "leaderBoardPlayers",
        "rankedStatus",
        "songSelectionRankingType",
        "rawStatus",
        "dir",
        "dl",
        "osu_90PP",
        "osu_95PP",
        "osu_96PP",
        "osu_97PP",
        "osu_98PP",
        "osu_99PP",
        "osu_SSPP",
        "osu_m90PP",
        "osu_m95PP",
        "osu_m96PP",
        "osu_m97PP",
        "osu_m98PP",
        "osu_m99PP",
        "osu_mSSPP",
        "accPpIfMapEndsNow",
        "aimPpIfMapEndsNow",
        "speedPpIfMapEndsNow",
        "strainPpIfMapEndsNow",
        "noChokePp",
        "skin",
        "skinPath",
        "songSelectionScores",
        "songSelectionMainPlayerScore",
        "songSelectionTotalScores",
        "status",
        "osu_99_9PP",
        "osu_m99_9PP",
        "mania_1_000_000PP",
        "mania_m1_000_000PP",
        "simulatedPp",
        "plays",
        "tags",
        "source",
        "starsNomod",
        "comboLeft",
        "localTime",
        "localTimeISO",
        "sl",
        "sv",
        "threadid",
        "test",
    ];

    /// Read the key order out of a serialised JSON object without a parser that
    /// would reorder it. `serde_json::Map` is a `BTreeMap` by default and would
    /// sort, so the bytes are walked directly.
    fn key_order(json: &str) -> Vec<String> {
        let bytes = json.as_bytes();
        let mut keys = Vec::new();
        let mut i = 1; // skip '{'
        while i < bytes.len() {
            if bytes[i] == b'"' {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                let raw = &json[start + 1..i];
                i += 1; // closing quote
                if i < bytes.len() && bytes[i] == b':' {
                    let mut depth = 0i32;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => {
                                if depth == 0 {
                                    break;
                                }
                                depth -= 1;
                            }
                            b',' if depth == 0 => break,
                            b'"' => {
                                i += 1;
                                while i < bytes.len() && bytes[i] != b'"' {
                                    i += 1;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    keys.push(raw.to_string());
                }
            } else {
                i += 1;
            }
        }
        keys
    }

    fn live_like_packet() -> crate::v2::TosuV2Packet {
        let mut packet = crate::v2::TosuV2Packet::default();
        packet.client = "stable".to_string();
        packet.state = OsuStatusState {
            number: 2,
            name: "play".to_string(),
        };
        packet.session.play_time = 5105;
        packet.beatmap.id = 2964306;
        packet.beatmap.set = 1404277;
        packet.beatmap.checksum = "85f9188803d1b6ae5ac107bdc17e7494".to_string();
        packet.beatmap.artist = "Morimori Atsushi".to_string();
        packet.beatmap.artist_unicode = "ãƒ¢ãƒªãƒ¢ãƒªã‚ã¤ã—".to_string();
        packet.beatmap.title = "Toono Gensou Monogatari (MRM REMIX)".to_string();
        packet.beatmap.title_unicode = "é é‡Žå¹»æƒ³ç‰©èªž (MRM REMIX)".to_string();
        packet.beatmap.version = "Extra".to_string();
        packet.beatmap.mapper = "-Syncro".to_string();
        packet.beatmap.folder =
            "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)".to_string();
        packet.beatmap.filename =
            "Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX) (-Syncro) [Extra].osu"
                .to_string();
        packet.beatmap.background_filename =
            "Chen_waifu2x_art_noise1_scale_tta_1 (1).png".to_string();
        packet.beatmap.audio_filename = "audio.mp3".to_string();
        packet.beatmap.mode = BeatmapMode {
            number: 0,
            name: "osu".to_string(),
        };
        // The capture's `keyOverlay` string, field for field:
        // `{"Enabled":true,"K1Pressed":false,"K1Count":11,"K2Pressed":false,
        // "K2Count":9,"M1Pressed":false,"M1Count":0,"M2Pressed":false,
        // "M2Count":0}`. Reproducing the two non-zero counts is what turns this
        // leaf from a recorded gap into a leaf rtosu can actually serve -- they
        // come from the key-overlay read, and osu! was paused mid-play, which is
        // why nothing was held down.
        packet.play.key_overlay = crate::v2::KeyOverlay {
            k1: crate::v2::KeyOverlayButton {
                is_pressed: false,
                count: 11,
            },
            k2: crate::v2::KeyOverlayButton {
                is_pressed: false,
                count: 9,
            },
            m1: crate::v2::KeyOverlayButton::UNPRESSED,
            m2: crate::v2::KeyOverlayButton::UNPRESSED,
        };
        // The capture was taken on a guest login, which is what the bancho block
        // reports: `UserLoginStatus.guest` is 256 and the id is -1
        // (`common/enums/osu.ts:80`).
        packet.profile.id = -1;
        packet.profile.name = "Guest".to_string();
        packet.profile.user_status = OsuStatusState {
            number: 256,
            name: "guest".to_string(),
        };
        packet.beatmap.time = crate::beatmap::BeatmapTime {
            live: 5105,
            first_object: 1134,
            last_object: 112584,
            mp3_length: 119433,
        };
        packet.beatmap.stats.bpm = crate::beatmap::BpmStats {
            realtime: 200.0,
            common: 200.0,
            min: 200.0,
            max: 200.0,
        };
        packet.beatmap.stats.ar = crate::beatmap::StatValue {
            original: 9.2,
            converted: 9.2,
        };
        packet.beatmap.stats.od = crate::beatmap::StatValue {
            original: 8.4,
            converted: 8.4,
        };
        packet.beatmap.stats.cs = crate::beatmap::StatValue {
            original: 3.8,
            converted: 3.8,
        };
        packet.beatmap.stats.hp = crate::beatmap::StatValue {
            original: 5.0,
            converted: 5.0,
        };
        packet.beatmap.stats.objects = crate::beatmap::ObjectCounts {
            circles: 318,
            sliders: 286,
            spinners: 0,
            holds: 0,
            total: 604,
        };
        packet.beatmap.stats.max_combo = 970;
        packet.folders.skin = "\u{2d} # re;owoTuna v1.1 \u{300e}Selyu\u{300f} # \u{2d}".to_string();
        packet.beatmap.preview_time = Some(72834);
        packet.beatmap.status = crate::beatmap::BeatmapStatus {
            number: 4,
            name: "ranked".to_string(),
        };
        packet.play.score = 4652;
        packet.play.mode.number = 0;
        packet.play.accuracy = 68.75;
        packet.play.health_bar.normal = 143.23868090259785 / 2.0;
        packet.play.health_bar.smooth = 144.70677448202431 / 2.0;
        packet.play.combo.current = 4;
        packet.play.combo.max = 11;
        packet.play.hits.n300 = 9;
        packet.play.hits.n100 = 6;
        packet.play.hits.n0 = 1;
        packet.play.unstable_rate = 231.25;
        packet.play.rank.current = "D".to_string();
        packet.play.rank.max_this_play = "A".to_string();
        packet.play.hit_error_array =
            std::sync::Arc::from([13, 21, 24, 32, 46, 52, 22, 23, 30, 33, -30, 1, -29, 27]);
        packet.play.mods = crate::v2::create_mods_state(1, "NF");
        packet.play.pp.current = 5.52;
        packet.play.pp.fc = 269.61;
        packet.play.pp.detailed.current = PpBreakdown {
            aim: 3.89,
            speed: 1.29,
            ..Default::default()
        };
        packet.performance.accuracy.n90 = 168.26;
        packet.performance.accuracy.n100 = 293.04;
        packet.performance.graph = crate::v2::PrecomputedGraph::new(&crate::v2::PerformanceGraph {
            series: vec![crate::v2::GraphSeries {
                name: "aim".to_string(),
                data: vec![-100.0, -50.0, 0.0, 122.14, 130.0],
            }],
            xaxis: vec![0.0, 400.0, 1134.0, 1534.0, 1934.0],
        });
        packet
    }

    #[test]
    fn the_payload_has_the_136_keys_tosu_serves_in_the_assembler_order() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let order = key_order(&serde_json::to_string(&sc).unwrap());
        assert_eq!(order.len(), 136, "key count");
        assert_eq!(order, EXPECTED_KEYS, "SC wire order");
    }

    /// `sc.ts` declares 200 lines and 144 leaves, and its declaration order is
    /// wrong in three places. Pinning the count alone would pass against a
    /// schema-ordered build; this asserts the specific keys that move.
    #[test]
    fn the_wire_order_is_the_assemblers_not_the_declared_interfaces() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let order = key_order(&serde_json::to_string(&sc).unwrap());

        let position = |key: &str| order.iter().position(|k| k == key).unwrap();
        // `sc.ts:9-10` declares banchoUsername before banchoId; the assembler
        // emits banchoIsConnected, banchoId, banchoUsername (`:59-61`).
        assert!(position("banchoId") < position("banchoUsername"));
        assert!(position("banchoIsConnected") < position("banchoId"));
        // `sc.ts:172-173` declares skinPath before skin; the assembler emits
        // skin, skinPath (`:274-275`).
        assert!(position("skin") < position("skinPath"));
        // `sc.ts:148-155` puts the 99_9 keys inside the pp block; the assembler
        // emits them last (`:284-287`).
        assert!(position("osu_99_9PP") > position("noChokePp"));
        assert!(position("mania_m1_000_000PP") > position("status"));
    }

    /// Five leaves are JSON wrapped in a **string**. Writing them as objects is
    /// the single easiest mistake in this payload, and the shape looks correct
    /// either way, so the *type* is what has to be pinned.
    #[test]
    fn the_five_stringified_leaves_are_strings_not_objects() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let json = serde_json::to_string(&sc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        for key in [
            "keyOverlay",
            "leaderBoardMainPlayer",
            "leaderBoardPlayers",
            "songSelectionScores",
            "songSelectionMainPlayerScore",
        ] {
            assert!(
                parsed[key].is_string(),
                "{key} must be a JSON string, got {}",
                kind_of(&parsed[key])
            );
        }
        // The other direction: the two that are genuinely arrays/objects.
        assert!(parsed["mapBreaks"].is_array());
        assert!(parsed["mapKiaiPoints"].is_array());
        assert!(parsed["mapStrains"].is_object());
        assert!(parsed["mapTimingPoints"].is_array());
        assert!(parsed["hitErrors"].is_array());
    }

    /// The strings have to be parseable and carry the assembler's key order, or
    /// a consumer's second `JSON.parse` loses the fields.
    #[test]
    fn the_stringified_leaves_parse_and_keep_their_inner_key_order() {
        let sc = ScPayload::from_v2(&live_like_packet());

        // Transcribed from `buildResultSC.ts:169-179`, with the capture's real
        // key counts rather than the neutral default: the inner key order and the
        // flattening into eight scalars are the contract, and the values are the
        // read's.
        assert_eq!(
            sc.key_overlay,
            r#"{"Enabled":true,"K1Pressed":false,"K1Count":11,"K2Pressed":false,"K2Count":9,"M1Pressed":false,"M1Count":0,"M2Pressed":false,"M2Count":0}"#
        );
        // Transcribed from `buildResultSC.ts:203-222`, with tosu's own `-1` XOR
        // constants. Fifteen keys -- the leading `IsLeaderboardVisible` is on
        // this shape and NOT on a `leaderBoardPlayers` entry.
        assert_eq!(
            sc.leader_board_main_player,
            r#"{"IsLeaderboardVisible":false,"Username":"","Score":0,"Combo":0,"MaxCombo":0,"Mods":{"ModsXor1":-1,"ModsXor2":-1,"Value":0},"Hit300":0,"Hit100":0,"Hit50":0,"HitMiss":0,"Team":0,"Position":0,"IsPassing":false}"#
        );
        // The list entries are a *different, shorter* shape. Asserting the count
        // difference is what keeps one struct from being used for both.
        let main: serde_json::Value = serde_json::from_str(&sc.leader_board_main_player).unwrap();
        let entry = serde_json::to_value(ScLeaderboardPlayer::default()).unwrap();
        assert_eq!(main.as_object().unwrap().len(), 13);
        assert_eq!(entry.as_object().unwrap().len(), 12);
        assert!(
            main.as_object()
                .unwrap()
                .contains_key("IsLeaderboardVisible")
        );
        assert!(
            !entry
                .as_object()
                .unwrap()
                .contains_key("IsLeaderboardVisible")
        );
        // And the key *order* is the assembler's, not alphabetical.
        assert_eq!(
            key_order(&sc.leader_board_main_player),
            [
                "IsLeaderboardVisible",
                "Username",
                "Score",
                "Combo",
                "MaxCombo",
                "Mods",
                "Hit300",
                "Hit100",
                "Hit50",
                "HitMiss",
                "Team",
                "Position",
                "IsPassing"
            ]
        );
        assert_eq!(sc.leader_board_players, "[]");
        assert_eq!(sc.song_selection_scores, "[]");
        assert_eq!(sc.song_selection_main_player_score, "{}");
    }

    /// `grade` and `maxGrade` are `GradeEnum` **indices**
    /// (`common/enums/osu.ts:1-11`), so `D` is 7 and `A` is 4. Writing the name
    /// is the natural mistake and the live capture is the only thing that
    /// catches it.
    #[test]
    fn grade_is_the_numeric_enum_not_the_name() {
        let mut packet = live_like_packet();
        packet.play.rank.current = "D".to_string();
        packet.play.rank.max_this_play = "A".to_string();
        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.grade, 7);
        assert_eq!(sc.max_grade, 4);

        let json = serde_json::to_string(&sc).unwrap();
        assert!(json.contains("\"grade\":7"), "{json}");
        assert!(json.contains("\"maxGrade\":4"), "{json}");

        // The whole enum, so a transposition is caught.
        for (index, name) in ["XH", "X", "SH", "S", "A", "B", "C", "D"]
            .iter()
            .enumerate()
        {
            assert_eq!(grade_enum_index(name), index as i32, "{name}");
        }
        // An empty grade is `GradeEnum.None`; tosu would serialise nothing at
        // all there, so the defined value is the closest match.
        assert_eq!(grade_enum_index(""), GRADE_NONE);
    }

    /// `time` is unrounded seconds while every other float in the payload goes
    /// through `fixDecimals`, and the millisecond strings are **unpadded**.
    /// Both are one-line mistakes that no type system catches.
    #[test]
    fn time_is_unrounded_seconds_and_the_millisecond_strings_are_unpadded() {
        let sc = ScPayload::from_v2(&live_like_packet());
        // 5105 ms -> 5.105, not 5.11.
        assert_eq!(sc.time, 5.105);
        let json = serde_json::to_string(&sc).unwrap();
        assert!(json.contains("\"time\":5.105"), "{json}");

        // `formatMilliseconds(5005)` is "00:00:05.5": the milliseconds field is
        // not zero-padded, which is upstream's bug and is the point. All six
        // expectations are what `node -e` prints for the same inputs, including
        // the negative case.
        assert_eq!(format_milliseconds(5005.0), "00:00:05.5");
        assert_eq!(format_milliseconds(5105.0), "00:00:05.105");
        assert_eq!(format_milliseconds(0.0), "00:00:00.0");
        assert_eq!(format_milliseconds(107_479.0), "00:01:47.479");
        assert_eq!(format_milliseconds(3_601_005.0), "01:00:01.5");
        assert_eq!(format_milliseconds(-1000.0), "-1:-1:-1.0");
        // Negative input is reproduced, not clamped: `timings.full - playTime`
        // goes negative past the last object, and tosu then pads the *negative*
        // hour/minute/second strings to two characters, which does nothing --
        // so `timeLeft` becomes `-1:-1:-1.0` rather than a zeroed clock.
        assert_eq!(format_milliseconds(-1.0), "-1:-1:-1.-1");

        // Both strings are present and are strings.
        assert_eq!(sc.map_position, "00:00:05.105");
        assert_eq!(sc.time_left, "00:01:47.479");
    }

    /// `mapStrains` is an object keyed by x-axis time, values clamped at zero,
    /// and the **keys** rendered as JavaScript numbers. The key text is the part
    /// that matters: `"0"` and `"0.0"` are different keys, so a consumer indexing
    /// `mapStrains["0"]` would miss.
    #[test]
    fn map_strains_is_a_zero_clamped_object_keyed_by_x_axis_time() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(
            sc.map_strains.points,
            vec![
                (0.0, -100.0),
                (400.0, -50.0),
                (1134.0, 0.0),
                (1534.0, 122.14),
                (1934.0, 130.0),
            ]
        );

        let json = serde_json::to_string(&sc).unwrap();
        let strains = json
            .split("\"mapStrains\":{")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("mapStrains object");
        // Keys are integral -- no ".0" anywhere -- and the negative padding tosu
        // inserts (`-100`/`-50`, `beatmap.ts:693-697`) is clamped to 0.
        let keys: Vec<&str> = strains
            .split(',')
            .map(|pair| pair.split(':').next().unwrap())
            .collect();
        assert_eq!(
            keys,
            ["\"0\"", "\"400\"", "\"1134\"", "\"1534\"", "\"1934\""]
        );

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let strains = &parsed["mapStrains"];
        assert_eq!(strains["0"], 0.0, "negative padding clamps to 0");
        assert_eq!(strains["400"], 0.0);
        assert_eq!(strains["1134"], 0.0);
        assert_eq!(strains["1534"], 122.14);
        assert_eq!(strains["1934"], 130.0);
    }

    /// `JSON.stringify` hoists array-index keys and sorts them ascending, ahead
    /// of every other key. Insertion order is deliberately scrambled here to
    /// prove the sort is real and not an accident of the input.
    #[test]
    fn integral_strain_keys_are_sorted_ascending_ahead_of_fractional_ones() {
        let strains = ScStrains {
            points: vec![
                (1934.0, 3.0),
                (2.5, 1.0),
                (0.0, 5.0),
                (400.0, 2.0),
                (1134.0, 4.0),
            ],
        };
        let json = serde_json::to_string(&strains).unwrap();
        // Read off the bytes, not through a parsed `Value`: `serde_json::Map` is
        // a `BTreeMap` and would sort, hiding exactly what this asserts.
        let order = key_order(&json);
        // Integral keys first, ascending, then the fractional one after them.
        assert_eq!(order, ["0", "400", "1134", "1934", "2.5"]);
    }

    #[test]
    fn js_number_drops_the_trailing_zero_js_would_drop() {
        assert_eq!(js_number(200.0), "200");
        assert_eq!(js_number(0.0), "0");
        assert_eq!(js_number(-50.0), "-50");
        assert_eq!(js_number(200.5), "200.5");
        // The equal-BPM form is unrounded on purpose: `maxBPM.toString()`.
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.m_bpm, "200");
        // The range form rounds, per `buildResultSC.ts:108`.
        let mut packet = live_like_packet();
        packet.beatmap.stats.bpm.min = 120.4;
        packet.beatmap.stats.bpm.max = 200.6;
        packet.beatmap.stats.bpm.common = 180.2;
        assert_eq!(ScPayload::from_v2(&packet).m_bpm, "120-201 (180)");
    }

    /// The four `osu_m*`/`m*` families are verbatim duplicates upstream, and the
    /// whole point of reproducing them is that they stay duplicates.
    #[test]
    fn the_m_prefixed_families_duplicate_their_plain_counterparts() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.osu_90pp, sc.osu_m90pp);
        assert_eq!(sc.osu_95pp, sc.osu_m95pp);
        assert_eq!(sc.osu_96pp, sc.osu_m96pp);
        assert_eq!(sc.osu_97pp, sc.osu_m97pp);
        assert_eq!(sc.osu_98pp, sc.osu_m98pp);
        assert_eq!(sc.osu_99pp, sc.osu_m99pp);
        assert_eq!(sc.osu_sspp, sc.osu_msspp);
        assert_eq!(sc.bpm, sc.m_main_bpm);
        assert_eq!(sc.main_bpm, sc.m_main_bpm);
        assert_eq!(sc.max_bpm, sc.m_max_bpm);
        assert_eq!(sc.min_bpm, sc.m_min_bpm);
        // `simulatedPp` is a third copy of `ppAcc[100]`.
        assert_eq!(sc.simulated_pp, sc.osu_sspp);
        // And `noChokePp` is a second copy of `fcPP`, upstream's own
        // `// TODO: idk if it's correct`.
        assert_eq!(sc.no_choke_pp, sc.pp_if_rest_fced);
    }

    /// `isBreakTime` is a hardcoded `0` upstream even though rtosu holds the
    /// real value, and `unstableRate` / `convertedUnstableRate` are the inverse
    /// of what their names suggest.
    #[test]
    fn is_break_time_stays_zero_and_the_unstable_rate_pair_is_not_inverted() {
        let mut packet = live_like_packet();
        packet.beatmap.is_break = true;
        packet.play.unstable_rate = 231.25;
        packet.play.mods.rate = 0.75;
        let sc = ScPayload::from_v2(&packet);
        // Correcting the hardcoded 0 would be a divergence, not a fix.
        assert_eq!(sc.is_break_time, 0);

        // rtosu's unstable_rate is already clock-rate divided, so it is the
        // *converted* one and the plain key scales it back up.
        assert_eq!(sc.converted_unstable_rate, 231.25);
        assert_eq!(sc.unstable_rate, 173.44);
    }

    /// `rawStatus` is osu!'s own `GameState`; `status` is SC's remapped enum.
    /// Putting the same number in both is a plausible mistake.
    #[test]
    fn status_is_remapped_but_raw_status_is_the_game_state() {
        // tosu's switch, state by state (`api/utils/scStatus.ts:14-35`).
        for (game_state, expected) in [
            (0, 1),   // menu -> Listening
            (1, 16),  // edit -> Editing
            (2, 2),   // play -> Playing
            (4, 16),  // selectEdit -> Editing
            (5, 1),   // selectPlay -> Listening
            (7, 32),  // resultScreen -> ResultsScreen
            (14, 32), // rankingVs -> ResultsScreen
            (17, 32), // rankingTagCoop -> ResultsScreen
            (18, 32), // rankingTeam -> ResultsScreen
            (22, 1),  // tourney -> Listening
            (23, 1),  // charts -> Listening
        ] {
            assert_eq!(
                stream_companion_status(game_state),
                expected,
                "{game_state}"
            );
        }

        let mut packet = live_like_packet();
        packet.state.number = 7;
        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.status, 32);
        assert_eq!(sc.raw_status, 7);
    }

    /// `mapKiaiPoints` is always `[]` upstream, and `mapTimingPoints` entries
    /// carry two keys against three in the schema.
    #[test]
    fn map_kiai_points_is_always_empty_and_timing_points_carry_two_keys() {
        let mut packet = live_like_packet();
        packet.beatmap.timing_points = vec![
            crate::beatmap::TimingPointSpan {
                time: 0,
                beat_length: 300.0,
            },
            crate::beatmap::TimingPointSpan {
                time: 60_000,
                beat_length: -150.0,
            },
        ];
        let sc = ScPayload::from_v2(&packet);
        assert!(sc.map_kiai_points.is_empty());

        let json = serde_json::to_string(&sc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Two keys per entry against three in `sc.ts`: `bpm` is declared
        // optional and `buildResultSC.ts:138-141` never writes it. A consumer
        // reading `point.bpm` therefore gets `undefined`, and reproducing the
        // key is the point.
        for point in parsed["mapTimingPoints"].as_array().unwrap() {
            assert_eq!(point.as_object().unwrap().len(), 2, "no bpm key: {point}");
        }
        assert_eq!(
            key_order(r#"{"startTime":0,"beatLength":300.0}"#),
            ["startTime", "beatLength"]
        );
        assert_eq!(parsed["mapTimingPoints"][0]["startTime"], 0);
        assert_eq!(parsed["mapTimingPoints"][0]["beatLength"], 300.0);
        // A redline keeps its negative declared length.
        assert_eq!(parsed["mapTimingPoints"][1]["beatLength"], -150.0);
        assert_eq!(parsed["mapTimingPoints"][1]["startTime"], 60000);
    }

    /// `ar`/`mAR` and friends are the raw and mods-converted values
    /// respectively -- the opposite of what the `m` might suggest.
    #[test]
    fn ar_and_mar_are_the_raw_and_converted_values() {
        let mut packet = live_like_packet();
        packet.beatmap.stats.ar.original = 9.3333;
        packet.beatmap.stats.ar.converted = 9.0;
        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.ar, 9.33, "raw, 2dp");
        assert_eq!(sc.m_ar, 9.0, "converted, 2dp");
    }

    /// Every leaf with no osu! stable source ships **tosu's own literal**
    /// (`validations/audits/audit-1.0.5.md` `M-03`), because a plausible-looking invented value is
    /// worse than an absent key. Fifteen leaves, all pinned here so a future
    /// pass cannot quietly replace one with a guess.
    #[test]
    fn the_fifteen_unsourceable_leaves_ship_tosus_literals() {
        let sc = ScPayload::from_v2(&live_like_packet());
        // M-03 1 and 2: chat-visibility and interface-visibility walks.
        assert_eq!(sc.chat_is_enabled, 0);
        assert_eq!(sc.ingame_interface_is_enabled, 0);
        // M-03 3: `gameplay.retries`.
        assert_eq!(sc.retries, 0);
        // M-03 7: `settings.leaderboardType`.
        assert_eq!(sc.song_selection_ranking_type, 0);
        // M-03 8 and 9: the mania 1M buckets.
        assert_eq!(sc.mania_1_000_000pp, -1);
        assert_eq!(sc.mania_m1_000_000pp, -1);
        // M-03 10: 99.9 % is not in tosu's table.
        assert_eq!(sc.osu_99_9pp, -1);
        assert_eq!(sc.osu_m99_9pp, -1);
        // M-03 11: stream identity.
        assert_eq!(sc.sl, -1);
        assert_eq!(sc.sv, -1);
        assert_eq!(sc.stars_nomod, -1);
        // M-03 12 and 13: chat identity and the debug hook.
        assert_eq!(sc.threadid, 0);
        assert_eq!(sc.test, "");
        // M-03 14: the song-selection score caches.
        assert_eq!(sc.song_selection_scores, "[]");
        assert_eq!(sc.song_selection_main_player_score, "{}");
        assert_eq!(sc.song_selection_total_scores, -1);
        // tosu's own `// TODO: we dont have that`, plus the two local-time keys.
        assert_eq!(sc.combo_left, -1);
        assert_eq!(sc.local_time, 0);
        assert_eq!(sc.local_time_iso, "0");
    }

    /// rtosu has real values for these seven and tosu hardcodes empties.
    /// `M-06` records the deviation as **not yet decided**; D4's default is to
    /// match tosu, so the literals are what ship, and this test is what makes
    /// flipping any one of them a deliberate act.
    #[test]
    fn the_seven_fields_where_rtosu_is_better_still_match_tosu() {
        let mut packet = live_like_packet();
        packet.beatmap.tags = "happy hardcore".to_string();
        packet.beatmap.source = "Artist Pack 12".to_string();
        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.plays, 0, "rtosu has a real play count");
        assert_eq!(sc.tags, "", "rtosu has the real .osu tags");
        assert_eq!(sc.source, "", "rtosu has the real .osu source");
        assert_eq!(sc.local_time, 0);
        assert_eq!(sc.local_time_iso, "0");
        assert_eq!(sc.combo_left, -1);
        assert_eq!(sc.stars_nomod, -1);
    }

    /// `playerHp` is the raw osu! value, so v2's `/200*100` bar has to be scaled
    /// back. Getting this wrong reports 71 where tosu reports 143. The two
    /// fixture bars are the exact halves of the live tosu values, so the
    /// multiplication has to return them digit for digit.
    #[test]
    fn player_hp_is_the_raw_value_not_the_bar_percentage() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.player_hp, 143.23868090259785);
        assert_eq!(sc.player_hp_smooth, 144.70677448202431);
        // The scale is exactly two, so the round trip is lossless.
        assert_eq!(sc.player_hp / 2.0 * 2.0, sc.player_hp);
        assert_eq!(sc.player_hp_smooth / 2.0 * 2.0, sc.player_hp_smooth);
    }

    /// `fixDecimals` folds a non-finite value to `0`, which is what tosu's
    /// `x || 0` does. A NaN pp value would otherwise serialise as `null` and
    /// break a consumer's arithmetic.
    #[test]
    fn fix_decimals_folds_non_finite_values_to_zero() {
        assert_eq!(fix_decimals(f32::NAN, 2), 0.0);
        assert_eq!(fix_decimals(f32::INFINITY, 2), 0.0);
        assert_eq!(fix_decimals(68.753, 2), 68.75);
        assert_eq!(fix_decimals(200.0, 4), 200.0);
    }

    /// Every pp-bearing leaf has to be present and finite. `M-05` records the
    /// per-leaf divergence as structural, so this asserts the *shape* is sound
    /// and never compares a magnitude.
    #[test]
    fn every_pp_leaf_is_present_and_finite() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let json = serde_json::to_string(&sc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "liveStarRating",
            "mStars",
            "ppIfMapEndsNow",
            "ppIfRestFced",
            "accPpIfMapEndsNow",
            "aimPpIfMapEndsNow",
            "speedPpIfMapEndsNow",
            "strainPpIfMapEndsNow",
            "noChokePp",
            "simulatedPp",
            "osu_90PP",
            "osu_95PP",
            "osu_96PP",
            "osu_97PP",
            "osu_98PP",
            "osu_99PP",
            "osu_SSPP",
        ] {
            let value = &parsed[key];
            assert!(value.is_f64() || value.is_i64(), "{key} is not a number");
            assert!(
                value.as_f64().is_some_and(f64::is_finite),
                "{key} is not finite"
            );
        }
    }

    /// `osuFileLocation` is the **.osu** file and `backgroundImageLocation` the
    /// background, both joined with a backslash. v1's `path.full` is the
    /// background under the same key name, so the two payloads legitimately
    /// disagree; each is transcribed from its own assembler.
    #[test]
    fn the_two_file_locations_join_folder_and_filename_with_a_backslash() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(
            sc.osu_file_location,
            "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)\\Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX) (-Syncro) [Extra].osu"
        );
        assert_eq!(
            sc.background_image_location,
            "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)\\Chen_waifu2x_art_noise1_scale_tta_1 (1).png"
        );
        // An empty side does not leave a dangling separator, which is
        // `path.join`'s behaviour and `join_path`'s.
        let mut packet = live_like_packet();
        packet.beatmap.folder = String::new();
        let sc = ScPayload::from_v2(&packet);
        assert!(!sc.osu_file_location.starts_with('\\'));
    }

    /// `dl` is built from the **map** id, not the set id, and the two differ on
    /// every map.
    #[test]
    fn dl_is_built_from_the_map_id() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.dl, "https://osu.ppy.sh/b/2964306");
        assert_ne!(sc.dl, format!("https://osu.ppy.sh/b/{}", sc.mapset_id));
    }

    /// `mapDiff` adds the brackets; `diffName` does not.
    #[test]
    fn map_diff_wraps_diff_name_in_brackets() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.diff_name, "Extra");
        assert_eq!(sc.map_diff, "[Extra]");
    }

    /// `banchoIsConnected` is a comparison against the raw login status
    /// constant `65793` (`UserLoginStatus.connected`), not a range check on the
    /// bancho status every other payload carries.
    #[test]
    fn bancho_is_connected_compares_against_the_connected_constant() {
        for (raw_login_status, expected) in [
            (65793, 1), // connected
            (256, 0),   // guest
            (257, 0),   // recieving_data
            (65537, 0), // disconnected
            (0, 0),     // reconnecting
        ] {
            let mut packet = live_like_packet();
            packet.profile.user_status = OsuStatusState {
                number: raw_login_status,
                name: String::new(),
            };
            let sc = ScPayload::from_v2(&packet);
            assert_eq!(sc.bancho_is_connected, expected, "{raw_login_status}");
            assert_eq!(sc.bancho_status, raw_login_status);
        }
    }

    /// `hitWindow` is not an SC leaf, so this asserts the new `#[serde(skip)]`
    /// beatmap fields cannot leak into **v2** -- the payload that already ships.
    #[test]
    fn the_new_beatmap_fields_do_not_leak_into_the_v2_packet() {
        let mut packet = live_like_packet();
        packet.beatmap.breaks = vec![crate::beatmap::BreakSpan {
            start_time: 34934,
            end_time: 36564,
            has_effect: true,
        }];
        packet.beatmap.timing_points = vec![crate::beatmap::TimingPointSpan {
            time: 0,
            beat_length: 300.0,
        }];
        packet.beatmap.preview_time = Some(72834);

        let v2 = serde_json::to_string(&packet).unwrap();
        for absent in [
            "\"breaks\"",
            "\"timingPoints\"",
            "\"previewTime\"",
            "\"preview_time\"",
        ] {
            assert!(!v2.contains(absent), "v2 must not gain {absent}");
        }
        // And the v2 key count is unchanged by the SC additions.
        let before = serde_json::to_string(&TosuV2Packet::default()).unwrap();
        assert_eq!(key_order(&v2).len(), key_order(&before).len());
    }

    /// A default packet has to serialise cleanly. A panic or a non-finite value
    /// here would take down the poll loop's only output path.
    /// Every leaf that is **not** a pp or a calculator value, pinned exactly
    /// against a live tosu `/json/sc` capture.
    ///
    /// The capture is `validations/snapshots/tosu_sc.json`: map 2964306
    /// "Toono Gensou Monogatari (MRM REMIX) [Extra]", osu!std, NF, paused a few
    /// seconds into the map, 16 objects judged. [`live_like_packet`] reproduces
    /// that state, so every expectation below is a value tosu really sent rather
    /// than one derived from reading its source.
    ///
    /// This is the "same values, in the same fields" check, and it is the only
    /// test that covers the *mapping* rather than the shape. A key-order bug is
    /// caught by the test above; a field wired to the wrong source -- the
    /// unicode artist, the mods bitfield, the `drainingtime` subtraction -- is
    /// caught only here.
    ///
    /// The pp-bearing leaves are deliberately absent. `G-08` records that the two
    /// calculators differ by construction, so asserting a magnitude would pin a
    /// number that is not supposed to match.
    #[test]
    fn every_non_pp_leaf_matches_the_live_capture_value_for_value() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let json = serde_json::to_string(&sc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let expect: &[(&str, serde_json::Value)] = &[
            // presence and bancho
            ("osuIsRunning", 1.into()),
            ("chatIsEnabled", 0.into()),
            ("ingameInterfaceIsEnabled", 0.into()),
            ("isBreakTime", 0.into()),
            ("banchoIsConnected", 0.into()),
            ("banchoId", (-1).into()),
            ("banchoUsername", "Guest".into()),
            ("banchoStatus", 256.into()),
            ("banchoCountry", "".into()),
            // metadata
            ("artistRoman", "Morimori Atsushi".into()),
            ("artistUnicode", "ãƒ¢ãƒªãƒ¢ãƒªã‚ã¤ã—".into()),
            ("titleRoman", "Toono Gensou Monogatari (MRM REMIX)".into()),
            (
                "titleUnicode",
                "é é‡Žå¹»æƒ³ç‰©èªž (MRM REMIX)".into(),
            ),
            (
                "mapArtistTitle",
                "Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)".into(),
            ),
            (
                "mapArtistTitleUnicode",
                "ãƒ¢ãƒªãƒ¢ãƒªã‚ã¤ã— - é é‡Žå¹»æƒ³ç‰©èªž (MRM REMIX)".into(),
            ),
            ("diffName", "Extra".into()),
            ("mapDiff", "[Extra]".into()),
            ("creator", "-Syncro".into()),
            // difficulty: raw vs converted, both post-`fixDecimals`
            ("ar", 9.2.into()),
            ("mAR", 9.2.into()),
            ("od", 8.4.into()),
            ("mOD", 8.4.into()),
            ("cs", 3.8.into()),
            ("mCS", 3.8.into()),
            ("hp", 5.into()),
            ("mHP", 5.into()),
            // bpm, 4dp, and the `m` family duplicating it
            ("currentBpm", 200.into()),
            ("bpm", 200.into()),
            ("mainBpm", 200.into()),
            ("maxBpm", 200.into()),
            ("minBpm", 200.into()),
            ("mMainBpm", 200.into()),
            ("mMaxBpm", 200.into()),
            ("mMinBpm", 200.into()),
            ("mBpm", "200".into()),
            // identity
            ("md5", "85f9188803d1b6ae5ac107bdc17e7494".into()),
            ("gameMode", "osu".into()),
            ("mode", 0.into()),
            // timing
            ("time", 5.105.into()),
            ("previewtime", 72834.into()),
            ("totaltime", 112584.into()),
            ("timeLeft", "00:01:47.479".into()),
            ("drainingtime", 111450.into()),
            ("totalAudioTime", 119433.into()),
            ("firstHitObjectTime", 1134.into()),
            // ids
            ("mapid", 2964306.into()),
            ("mapsetid", 1404277.into()),
            ("mapKiaiPoints", serde_json::json!([])),
            ("mapPosition", "00:00:05.105".into()),
            // object counts
            ("sliders", 286.into()),
            ("circles", 318.into()),
            ("spinners", 0.into()),
            // files
            ("mp3Name", "audio.mp3".into()),
            (
                "osuFileName",
                "Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX) (-Syncro) [Extra].osu"
                    .into(),
            ),
            (
                "backgroundImageFileName",
                "Chen_waifu2x_art_noise1_scale_tta_1 (1).png".into(),
            ),
            (
                "osuFileLocation",
                "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)\\Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX) (-Syncro) [Extra].osu".into(),
            ),
            (
                "backgroundImageLocation",
                "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)\\Chen_waifu2x_art_noise1_scale_tta_1 (1).png".into(),
            ),
            // play state
            ("retries", 0.into()),
            ("username", "".into()),
            ("score", 4652.into()),
            ("playerHp", 143.23868090259785.into()),
            ("playerHpSmooth", 144.70677448202431.into()),
            ("combo", 4.into()),
            ("currentMaxCombo", 11.into()),
            // hits
            ("geki", 0.into()),
            ("c300", 9.into()),
            ("katsu", 0.into()),
            ("c100", 6.into()),
            ("c50", 0.into()),
            ("miss", 1.into()),
            ("sliderBreaks", 0.into()),
            ("acc", 68.75.into()),
            ("unstableRate", 231.25.into()),
            ("convertedUnstableRate", 231.25.into()),
            // the numeric GradeEnum
            ("grade", 7.into()),
            ("maxGrade", 4.into()),
            (
                "hitErrors",
                serde_json::json!([13, 21, 24, 32, 46, 52, 22, 23, 30, 33, -30, 1, -29, 27]),
            ),
            // mods
            ("mods", "NF".into()),
            ("modsEnum", 1.into()),
            // leaderboard
            ("leaderBoardPlayers", "[]".into()),
            // status
            ("rankedStatus", 4.into()),
            ("songSelectionRankingType", 0.into()),
            ("rawStatus", 2.into()),
            (
                "dir",
                "1404277 Morimori Atsushi - Toono Gensou Monogatari (MRM REMIX)".into(),
            ),
            ("dl", "https://osu.ppy.sh/b/2964306".into()),
            // the four keys tosu emits last
            ("osu_99_9PP", (-1).into()),
            ("osu_m99_9PP", (-1).into()),
            ("mania_1_000_000PP", (-1).into()),
            ("mania_m1_000_000PP", (-1).into()),
            // fields where rtosu has better values and still matches tosu
            ("plays", 0.into()),
            ("tags", "".into()),
            ("source", "".into()),
            ("starsNomod", (-1).into()),
            ("comboLeft", (-1).into()),
            ("localTime", 0.into()),
            ("localTimeISO", "0".into()),
            ("sl", (-1).into()),
            ("sv", (-1).into()),
            ("threadid", 0.into()),
            ("test", "".into()),
            // song-selection caches and SC's remapped status
            ("songSelectionScores", "[]".into()),
            ("songSelectionMainPlayerScore", "{}".into()),
            ("songSelectionTotalScores", (-1).into()),
            ("status", 2.into()),
            // `skin` and `skinPath` are the same string twice upstream
            // (`buildResultSC.ts:274-275`), taken from the real skin folder
            // rather than fabricated.
            ("skin", "\u{2d} # re;owoTuna v1.1 \u{300e}Selyu\u{300f} # \u{2d}".into()),
            ("skinPath", "\u{2d} # re;owoTuna v1.1 \u{300e}Selyu\u{300f} # \u{2d}".into()),
            // The key overlay, verbatim from the capture. This leaf was on the
            // "cannot compare" list until the read landed
            // (`client::read_key_overlay`); with the read in place the whole
            // string is a value rtosu produces, so it is pinned here instead.
            (
                "keyOverlay",
                r#"{"Enabled":true,"K1Pressed":false,"K1Count":11,"K2Pressed":false,"K2Count":9,"M1Pressed":false,"M1Count":0,"M2Pressed":false,"M2Count":0}"#
                    .into(),
            ),
        ];

        for (key, expected) in expect {
            assert!(
                json_equal(&parsed[*key], expected),
                "{key} does not match the live tosu capture: got {}, expected {expected}",
                parsed[*key]
            );
        }

        // The table has to be the *whole* comparable surface, or a new field
        // could ship unpinned. Anything absent from it must be named below with
        // the reason it cannot be compared -- so adding a leaf to the payload
        // fails here until it is either pinned or explained.
        let pinned: std::collections::BTreeSet<&str> = expect.iter().map(|(k, _)| *k).collect();
        let mut unpinned: Vec<&str> = EXPECTED_KEYS
            .iter()
            .copied()
            .filter(|key| !pinned.contains(key))
            .collect();
        unpinned.sort_unstable();
        let mut named = vec![
            // 24 pp-bearing leaves. `G-08`: rosu-pp-gemini 5.0.1 against
            // tosu's prebuilt lazer calculator, two implementations of one
            // specification. No constant is fitted anywhere in this file, so
            // a magnitude assertion here would pin a number that is not
            // supposed to match.
            "liveStarRating",
            "mStars",
            "ppIfMapEndsNow",
            "ppIfRestFced",
            "noChokePp",
            "simulatedPp",
            "accPpIfMapEndsNow",
            "aimPpIfMapEndsNow",
            "speedPpIfMapEndsNow",
            "strainPpIfMapEndsNow",
            "osu_90PP",
            "osu_95PP",
            "osu_96PP",
            "osu_97PP",
            "osu_98PP",
            "osu_99PP",
            "osu_SSPP",
            "osu_m90PP",
            "osu_m95PP",
            "osu_m96PP",
            "osu_m97PP",
            "osu_m98PP",
            "osu_m99PP",
            "osu_mSSPP",
            // `maxCombo` comes from rosu, and `F-10` records that rtosu does
            // not apply tosu's classic (`CL`) mod, so the two calculators
            // disagree on it for every map.
            "maxCombo",
            // `mapStrains` is the strain graph, and `G-01` records that
            // rtosu's geometry still omits tosu's `OFFSET_L`/`OFFSET_R` and
            // the `-50` runs, so the key set and length differ. The captured
            // key *shape* is pinned in `map_strains_keys_are_the_captured_shape`.
            "mapStrains",
            // `mapTimingPoints[].startTime` is lazer's `group.startTime` and
            // `mapBreaks` needs a real map; both are covered structurally in
            // `the_leaves_that_need_live_data_record_what_the_capture_saw`.
            "mapTimingPoints",
            "mapBreaks",
            // No rtosu read exists: `I-10`/`L-09` for the scoreboard. The
            // captured values are recorded in the test named above.
            //
            // `keyOverlay` used to be on this list. It is not any more -- the
            // read exists (`client::read_key_overlay`) and the leaf is pinned to
            // the capture in `the_stringified_leaves_parse_and_keep_their_inner_key_order`.
            "leaderBoardMainPlayer",
        ];
        named.sort_unstable();
        assert_eq!(
            unpinned, named,
            "every SC leaf is either pinned to the live capture or named here"
        );
    }

    /// The five values the fixture cannot reproduce from a static packet, pinned
    /// separately because each needs live data rtosu does not read.
    ///
    /// The capture's values are recorded so a future pass can see what was
    /// actually observed rather than inferring it.
    #[test]
    fn the_leaves_that_need_live_data_record_what_the_capture_saw() {
        // The capture's `skin` and `skinPath` were both
        // "- # re;owoTuna v1.1 \u{300e}Selyu\u{300f} # -", the same string twice
        // (`buildResultSC.ts:274-275`).
        let mut packet = live_like_packet();
        packet.folders.skin = "skin-from-memory".to_string();
        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.skin, "skin-from-memory");
        assert_eq!(sc.skin_path, sc.skin, "tosu sends one string for both");

        // `keyOverlay` in the capture had non-zero counts (K1Count 11,
        // K2Count 9), read from osu! stable (`memory/stable.ts:644-728`). rtosu
        // has that read now (`client::read_key_overlay`), so the fixture's values
        // reach the string instead of the neutral default -- which is why this
        // leaf is no longer on the unpinned list.
        let overlay: serde_json::Value = serde_json::from_str(&sc.key_overlay).unwrap();
        assert_eq!(
            overlay["K1Count"], 11,
            "the capture's count, carried through"
        );
        assert_eq!(
            overlay["K2Count"], 9,
            "the capture's count, carried through"
        );
        assert_eq!(overlay["Enabled"], true);

        // `leaderBoardMainPlayer` in the capture was all-zero with the `-1` XOR
        // words, and `leaderBoardPlayers` was `[]`, because no scoreboard was
        // loaded. rtosu reports the same shape unconditionally.
        let main: serde_json::Value = serde_json::from_str(&sc.leader_board_main_player).unwrap();
        assert_eq!(main["Username"], "");
        assert_eq!(main["Mods"]["ModsXor1"], -1);
        assert_eq!(main["Mods"]["ModsXor2"], -1);
        assert_eq!(sc.leader_board_players, "[]");

        // `mapBreaks` in the capture had three spans; `mapTimingPoints` had one
        // with `startTime` equal to the first object's time, which is lazer's
        // `group.startTime` and the one field rtosu reports differently.
        let mut packet = live_like_packet();
        packet.beatmap.breaks = vec![
            crate::beatmap::BreakSpan {
                start_time: 34934,
                end_time: 36564,
                has_effect: true,
            },
            crate::beatmap::BreakSpan {
                start_time: 62000,
                end_time: 64000,
                has_effect: false,
            },
        ];
        packet.beatmap.timing_points = vec![crate::beatmap::TimingPointSpan {
            time: 0,
            beat_length: 300.0,
        }];
        let sc = ScPayload::from_v2(&packet);
        let json = serde_json::to_string(&sc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["mapBreaks"][0]["startTime"], 34934);
        assert_eq!(parsed["mapBreaks"][0]["endTime"], 36564);
        assert_eq!(parsed["mapBreaks"][0]["hasEffect"], true);
        assert_eq!(parsed["mapBreaks"][1]["hasEffect"], false);
        // The captured `beatLength` was 300 for a 200 BPM map; `startTime` was
        // 1134 (the first object's time) in the capture and is 0 here, which is
        // the documented divergence in `TimingPointSpan::time`.
        assert_eq!(parsed["mapTimingPoints"][0]["beatLength"], 300.0);
        assert_eq!(parsed["mapTimingPoints"][0]["startTime"], 0);
    }

    /// `mapStrains` in the capture had 298 keys, starting `0, 400, 1134, 1534,
    /// 1934` -- the leading `-100`/`-50` padding, which the value clamp turns
    /// into zeros. The exact length depends on the map and is rtosu's graph
    /// geometry (`validations/audits/audit-1.0.5.md` `G-01`, still open), so it is not pinned here;
    /// the key *shape* is.
    #[test]
    fn map_strains_keys_are_the_captured_shape() {
        let sc = ScPayload::from_v2(&live_like_packet());
        let json = serde_json::to_string(&sc).unwrap();
        // Off the bytes, not through a parsed `Value`: `serde_json::Map` is a
        // `BTreeMap` and would sort "400" before "1134".
        let object = json
            .split("\"mapStrains\":{")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("mapStrains object");
        assert_eq!(
            key_order(&format!("{{{object}}}")),
            ["0", "400", "1134", "1534", "1934"]
        );
    }

    /// Every expectation here is the literal output of
    /// `validations/js_semantics.mjs`, which runs tosu's own
    /// `formatMilliseconds` (`packages/common/utils/manipulation.ts:4-17`)
    /// verbatim under node. The negative rows are not hypothetical: a live
    /// capture with osu! paused in play had `playTime` run away to ~59 million
    /// ms, so `timeLeft` was a deeply negative clock.
    ///
    /// The three ways a hand-written port goes wrong are all covered: the
    /// **unpadded** milliseconds, the **negative** hours/minutes/seconds that
    /// `padStart(2, '0')` does not fix, and JavaScript's **negative zero**
    /// (`-1000 % 1000` is `-0`, and `String(-0)` is `"0"`, where Rust's
    /// `{:.0}` would print `"-0"`).
    #[test]
    fn format_milliseconds_matches_the_javascript_reference_on_every_shape() {
        // (input ms, node's output)
        let cases: &[(f64, &str)] = &[
            (0.0, "00:00:00.0"),
            (5.0, "00:00:00.5"),
            (1005.0, "00:00:01.5"),
            (5005.0, "00:00:05.5"),
            (5105.0, "00:00:05.105"),
            (5100.0, "00:00:05.100"),
            (107_479.0, "00:01:47.479"),
            (3_601_005.0, "01:00:01.5"),
            (-1.0, "-1:-1:-1.-1"),
            (-1000.0, "-1:-1:-1.0"),
            (-53_540.0, "-1:-1:-54.-540"),
            (-58_951_629.0, "-17:-23:-32.-629"),
        ];
        for (ms, expected) in cases {
            assert_eq!(
                &format_milliseconds(*ms),
                expected,
                "node disagrees for {ms} ms"
            );
        }
    }

    /// The same reference for [`js_number`], which produces the `mBpm` string
    /// and every `mapStrains` key.
    #[test]
    fn js_number_matches_the_javascript_reference() {
        for (value, expected) in [
            (0.0, "0"),
            (200.0, "200"),
            (-50.0, "-50"),
            (200.5, "200.5"),
            (120.4, "120.4"),
            (200.6, "200.6"),
            (180.2, "180.2"),
            (1134.0, "1134"),
        ] {
            assert_eq!(js_number(value), expected, "node disagrees for {value}");
        }
    }

    /// The equal-BPM form is unrounded on purpose: `maxBPM.toString()`
    /// (`buildResultSC.ts:107`). The range form rounds each part with
    /// `Math.round` (`:108`).
    #[test]
    fn m_bpm_uses_the_two_forms_tosu_writes() {
        let sc = ScPayload::from_v2(&live_like_packet());
        assert_eq!(sc.m_bpm, "200", "min == max is unrounded");

        let mut packet = live_like_packet();
        packet.beatmap.stats.bpm.min = 120.4;
        packet.beatmap.stats.bpm.max = 200.6;
        packet.beatmap.stats.bpm.common = 180.2;
        assert_eq!(ScPayload::from_v2(&packet).m_bpm, "120-201 (180)");
    }

    /// The playhead for `time`, `timeLeft` and `mapPosition` is
    /// `beatmap.time.live` -- **not** `session.play_time`.
    ///
    /// tosu's v2 exposes the same quantity twice under two names
    /// (`buildResultV2.ts:147` and `:333`), and the SC builder wants the other
    /// one. Live, on a paused osu! in play state, the two read 5105 and
    /// 59,064,147 respectively: the first is the song position and the second a
    /// whole-session counter. Using the wrong one put `time` 16 hours ahead of
    /// the real playhead and turned `timeLeft` into `-17:-39:-37.-430`.
    ///
    /// The fixture sets both fields to different values, so swapping them back
    /// fails here rather than only showing up in a live diff.
    #[test]
    fn the_playhead_is_the_beatmap_live_time_not_the_session_counter() {
        let mut packet = live_like_packet();
        packet.beatmap.time.live = 5105;
        packet.session.play_time = 59_064_147;

        let sc = ScPayload::from_v2(&packet);
        assert_eq!(sc.time, 5.105);
        assert_eq!(sc.map_position, "00:00:05.105");
        // 112584 - 5105 = 107479, not 112584 - 59064147.
        assert_eq!(sc.time_left, "00:01:47.479");
        assert_ne!(
            sc.time_left, "-17:-23:-32.-629",
            "session.play_time leaked back in"
        );
    }

    #[test]
    fn a_default_packet_still_produces_valid_json() {
        let sc = ScPayload::from_v2(&TosuV2Packet::default());
        let json = serde_json::to_string(&sc).expect("default SC payload serialises");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(key_order(&json).len(), 136);
        assert_eq!(parsed["osuIsRunning"], 1);
        assert_eq!(parsed["previewtime"], -1, "an unresolved PreviewTime is -1");
        assert_eq!(parsed["mapStrains"], serde_json::json!({}));
        assert_eq!(parsed["hitWindow"], serde_json::Value::Null);
    }

    fn kind_of(value: &serde_json::Value) -> &'static str {
        match value {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    /// Compare two JSON values the way a consumer would: numbers by value, not
    /// by the text that produced them.
    ///
    /// `serde_json::Value` distinguishes `Number(5)` from `Number(5.0)`, and
    /// `serde_json` renders rtosu's floats as `5.0` where `JSON.stringify` emits
    /// `5`. That is a real byte difference and a non-difference at the same time
    /// -- see the module doc -- so a value table built from a tosu capture has to
    /// compare numerically or it fails on every integral float.
    fn json_equal(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
        match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => actual == expected,
        }
    }

    /// Guards the test helper itself: if `key_order` stopped reading order off
    /// the bytes it would report the same thing for every shape and every order
    /// assertion above would pass vacuously.
    #[test]
    fn the_key_order_helper_reads_order_and_not_just_membership() {
        assert_eq!(key_order(r#"{"b":1,"a":2}"#), vec!["b", "a"]);
        assert_eq!(
            key_order(r#"{"x":{"b":1,"a":2},"y":[3,4]}"#),
            vec!["x", "y"],
            "nested keys are not top-level keys"
        );
        assert_eq!(key_order(r#"{"0":1,"1":2}"#), vec!["0", "1"]);
    }
}
