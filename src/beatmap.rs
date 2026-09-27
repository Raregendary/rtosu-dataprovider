use crate::address::checked_add_signed;
use crate::client::mod_bits;
use crate::process::ProcessMemory;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapTime {
    pub live: i32,
    pub first_object: i32,
    pub last_object: i32,
    pub mp3_length: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapStatus {
    pub number: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapMode {
    pub number: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StarsBreakdown {
    pub live: f32,
    pub aim: f32,
    pub speed: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashlight: Option<f32>,
    pub slider_factor: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamina: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rhythm: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<f32>,
    pub reading: f32,
    pub hit_window: f32,
    pub total: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatValue {
    pub original: f32,
    pub converted: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BpmStats {
    pub realtime: f32,
    pub common: f32,
    pub min: f32,
    pub max: f32,
}

/// The unclocked BPM values parsed out of the `.osu` timing points.
///
/// [`BpmStats`] holds the *played* values, which the clock rate scales, so
/// scaling one of its fields in place would be a double scale on the next call.
/// Keeping the parsed values here makes the conversion a pure function of
/// (base, clock rate). Only [`populate_beatmap_file_metadata`] writes this, and
/// only the three fields it parses: `common` is taken from the rosu beatmap
/// instead, so a base for it would never be read.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BpmBase {
    pub realtime: f32,
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCounts {
    pub circles: i32,
    pub sliders: i32,
    pub spinners: i32,
    pub holds: i32,
    pub total: i32,
}

/// One `[Events] Break` span, in the shape SC's `mapBreaks` needs.
///
/// Parsed from the `.osu` file rather than from rosu's beatmap so it is available
/// without the `pp` feature, the same file pass that already reads the object
/// and timing-point sections.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BreakSpan {
    pub start_time: i32,
    pub end_time: i32,
    pub has_effect: bool,
}

/// One `[TimingPoints]` line: the raw declared beat length, negative for a
/// redline. This is lazer's `TimingChangePoint.beatLength`, so SC's
/// `mapTimingPoints[].beatLength` matches tosu on the value; see the `time`
/// field for the one leaf it does not.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TimingPointSpan {
    /// The timing point's own time, **not** lazer's `group.startTime`.
    ///
    /// tosu emits `r.group?.startTime || 0`
    /// (`tosu-sourcecode/packages/tosu/src/api/utils/buildResultSC.ts:138-141`),
    /// which is the first object inheriting the timing group -- equal to the
    /// point's own time only for the first point of a map. rtosu does not model
    /// lazer's grouping, so this reports the point's own time. The divergence is
    /// deliberate and is documented at the SC builder rather than papered over,
    /// because the alternative is reporting `0` for every point outside a group
    /// start, which is lossy in the other direction.
    pub time: i32,
    pub beat_length: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HitWindowState {
    pub miss: f64,
    pub meh: f64,
    pub ok: f64,
    pub great: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapPpStats {
    pub ss: f32,
    pub fc: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapStats {
    pub stars: StarsBreakdown,
    pub ar: StatValue,
    pub cs: StatValue,
    pub od: StatValue,
    pub hp: StatValue,
    pub bpm: BpmStats,
    #[serde(skip)]
    pub bpm_base: BpmBase,
    pub objects: ObjectCounts,
    pub hit_window: HitWindowState,
    pub max_combo: i32,
    #[serde(skip)]
    pub pp: BeatmapPpStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapSnapshot {
    pub is_kiai: bool,
    pub is_break: bool,
    pub is_convert: bool,
    pub time: BeatmapTime,
    pub status: BeatmapStatus,
    pub checksum: String,
    pub id: i32,
    pub set: i32,
    pub mode: BeatmapMode,
    pub artist: String,
    pub artist_unicode: String,
    pub title: String,
    pub title_unicode: String,
    pub mapper: String,
    pub version: String,
    #[serde(skip)]
    pub folder: String,
    #[serde(skip)]
    pub filename: String,
    #[serde(skip)]
    pub audio_filename: String,
    #[serde(skip)]
    pub background_filename: String,
    pub source: String,
    pub tags: String,
    pub stats: BeatmapStats,
    /// `[Events] Break` spans. `#[serde(skip)]` because this is the v2 packet and
    /// tosu's v2 payload has no such key -- it belongs only to the SC leaf
    /// `mapBreaks` (`buildResultSC.ts:131-135`).
    #[serde(skip)]
    pub breaks: Vec<BreakSpan>,
    /// `[TimingPoints]` lines, for the SC leaf `mapTimingPoints` only
    /// (`buildResultSC.ts:138-141`). Skipped for the same reason.
    #[serde(skip)]
    pub timing_points: Vec<TimingPointSpan>,
    /// `[General] PreviewTime`, for the SC leaf `previewtime`
    /// (`buildResultSC.ts:116`, from `states/beatmap.ts:488`). `None` means the
    /// line was absent, which osu!lazer reports as `-1`; `Some(-1)` is an
    /// explicit `PreviewTime:-1`. Both serialise as `-1`.
    #[serde(skip)]
    pub preview_time: Option<i32>,
}

pub fn beatmap_status_name(status: i32) -> &'static str {
    match status {
        0 => "unknown",
        1 => "notSubmitted",
        2 => "pending",
        3 => "unused",
        4 => "ranked",
        5 => "approved",
        6 => "qualified",
        7 => "loved",
        _ => "unknown",
    }
}

pub fn beatmap_mode_name(mode: i32) -> &'static str {
    match mode {
        0 => "osu",
        1 => "taiko",
        2 => "fruits",
        3 => "mania",
        _ => "osu",
    }
}

/// Read the active beatmap pointer address from osu! process memory.
/// Returns 0 if no beatmap is loaded.
pub fn read_beatmap_ptr(memory: &ProcessMemory, base_addr: u64) -> Result<u64> {
    let beatmap_ptr_addr = checked_add_signed(base_addr, -0xc)?;
    memory
        .read_indirect_pointer(beatmap_ptr_addr)
        .context("reading beatmap pointer")
}

/// Read the current audio playback time in milliseconds.
pub fn read_live_time(memory: &ProcessMemory, play_time_addr: Option<u64>) -> i32 {
    if let Some(pt_addr) = play_time_addr {
        let time_ptr_addr = checked_add_signed(pt_addr, 0x5).unwrap_or(pt_addr);
        let time_ptr = memory.read_pointer(time_ptr_addr).unwrap_or(0);
        if time_ptr != 0 {
            memory.read_i32(time_ptr).unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    }
}

/// Read only the beatmap id from an already resolved beatmap pointer.
///
/// A single integer at a fixed offset, so it can be polled every tick to detect
/// a map change without allocating. The checksum would work too, but reading it
/// means building a String sixty times a second for nothing.
pub fn read_beatmap_id(memory: &ProcessMemory, beatmap_addr: u64) -> i32 {
    if beatmap_addr == 0 {
        return 0;
    }
    let Ok(addr) = checked_add_signed(beatmap_addr, 0xC8) else {
        return 0;
    };
    memory.read_i32(addr).unwrap_or(0)
}

/// Whether a cached beatmap snapshot must be re-read.
///
/// osu! stable reuses the same beatmap object when the map changes, so a stable
/// pointer is not proof of a stable map. The id is a single integer at a fixed
/// offset (see [`read_beatmap_id`]) and costs one read per tick. A `live_id` of
/// zero or less means the id has not resolved yet, so only a pointer change
/// counts: acting on an unresolved id would re-read a map that has not changed.
pub fn beatmap_refresh_needed(ptr: u64, cached_ptr: u64, live_id: i32, cached_id: i32) -> bool {
    ptr != cached_ptr || (live_id > 0 && live_id != cached_id)
}

/// Read beatmap details from an already resolved beatmap pointer address.
pub fn read_beatmap_from_ptr(
    memory: &ProcessMemory,
    beatmap_addr: u64,
    base_addr: u64,
    live_time: i32,
    pointer_width: usize,
) -> Result<BeatmapSnapshot> {
    crate::instr_scope!(BeatmapMemory);
    if beatmap_addr == 0 {
        return Ok(BeatmapSnapshot::default());
    }

    let id = memory
        .read_i32(checked_add_signed(beatmap_addr, 0xC8)?)
        .unwrap_or(0);
    let set_id = memory
        .read_i32(checked_add_signed(beatmap_addr, 0xCC)?)
        .unwrap_or(0);
    let status_raw = memory
        .read_i16(checked_add_signed(beatmap_addr, 0x12C)?)
        .unwrap_or(0) as i32;
    let status_num = if (1..=7).contains(&status_raw) {
        status_raw
    } else {
        0
    };
    let mode = memory
        .read_indirect_pointer(checked_add_signed(base_addr, -0x33)?)
        .unwrap_or(0) as i32;
    let object_count = memory
        .read_i32(checked_add_signed(beatmap_addr, 0xF8)?)
        .unwrap_or(0);

    let ar = memory
        .read_f32(checked_add_signed(beatmap_addr, 0x2C)?)
        .unwrap_or(0.0);
    let cs = memory
        .read_f32(checked_add_signed(beatmap_addr, 0x30)?)
        .unwrap_or(0.0);
    let hp = memory
        .read_f32(checked_add_signed(beatmap_addr, 0x34)?)
        .unwrap_or(0.0);
    let od = memory
        .read_f32(checked_add_signed(beatmap_addr, 0x38)?)
        .unwrap_or(0.0);

    let artist = read_net_string(memory, beatmap_addr, 0x18, pointer_width).unwrap_or_default();
    let artist_unicode =
        read_net_string(memory, beatmap_addr, 0x1C, pointer_width).unwrap_or_default();
    let title = read_net_string(memory, beatmap_addr, 0x24, pointer_width).unwrap_or_default();
    let title_unicode =
        read_net_string(memory, beatmap_addr, 0x28, pointer_width).unwrap_or_default();
    let audio_filename =
        read_net_string(memory, beatmap_addr, 0x64, pointer_width).unwrap_or_default();
    let background_filename =
        read_net_string(memory, beatmap_addr, 0x68, pointer_width).unwrap_or_default();
    let checksum = read_net_string(memory, beatmap_addr, 0x6C, pointer_width).unwrap_or_default();
    let folder = read_net_string(memory, beatmap_addr, 0x78, pointer_width).unwrap_or_default();
    let mapper = read_net_string(memory, beatmap_addr, 0x7C, pointer_width).unwrap_or_default();
    let filename = read_net_string(memory, beatmap_addr, 0x90, pointer_width).unwrap_or_default();
    let version = read_net_string(memory, beatmap_addr, 0xAC, pointer_width).unwrap_or_default();

    Ok(BeatmapSnapshot {
        is_kiai: false,
        is_break: false,
        is_convert: false,
        time: BeatmapTime {
            live: live_time,
            first_object: 0,
            last_object: 0,
            mp3_length: 0,
        },
        status: BeatmapStatus {
            number: status_num,
            name: beatmap_status_name(status_num).to_string(),
        },
        checksum,
        id,
        set: set_id,
        mode: BeatmapMode {
            number: mode,
            name: beatmap_mode_name(mode).to_string(),
        },
        artist,
        artist_unicode,
        title,
        title_unicode,
        mapper,
        version,
        folder,
        filename,
        background_filename,
        audio_filename,
        // Filled in by `populate_beatmap_file_metadata`, which is the only thing
        // that reads the `.osu` file. A snapshot built straight from memory
        // carries none, which is the same state the SC builder reports `-1` for.
        breaks: Vec::new(),
        timing_points: Vec::new(),
        preview_time: None,
        stats: BeatmapStats {
            stars: StarsBreakdown {
                live: 0.0,
                aim: 0.0,
                speed: 0.0,
                flashlight: None,
                slider_factor: 0.0,
                stamina: None,
                rhythm: None,
                color: None,
                reading: 0.0,
                hit_window: 0.0,
                total: 0.0,
            },
            ar: StatValue {
                original: ar,
                converted: ar,
            },
            cs: StatValue {
                original: cs,
                converted: cs,
            },
            od: StatValue {
                original: od,
                converted: od,
            },
            hp: StatValue {
                original: hp,
                converted: hp,
            },
            bpm: BpmStats {
                realtime: 0.0,
                common: 0.0,
                min: 0.0,
                max: 0.0,
            },
            bpm_base: BpmBase::default(),
            objects: ObjectCounts {
                circles: 0,
                sliders: 0,
                spinners: 0,
                holds: 0,
                total: object_count,
            },
            hit_window: HitWindowState::default(),
            max_combo: 0,
            pp: BeatmapPpStats { ss: 0.0, fc: 0.0 },
        },
        source: String::new(),
        tags: String::new(),
    })
}

/// Read active beatmap metadata from osu! process memory given the base pointer address.
pub fn read_beatmap_memory(
    memory: &ProcessMemory,
    base_addr: u64,
    play_time_addr: Option<u64>,
    pointer_width: usize,
) -> Result<BeatmapSnapshot> {
    let beatmap_addr = read_beatmap_ptr(memory, base_addr)?;
    if beatmap_addr == 0 {
        return Ok(BeatmapSnapshot::default());
    }
    let live_time = read_live_time(memory, play_time_addr);
    read_beatmap_from_ptr(memory, beatmap_addr, base_addr, live_time, pointer_width)
}

pub fn populate_beatmap_file_metadata(snapshot: &mut BeatmapSnapshot, path: &Path) -> bool {
    crate::instr_scope!(BeatmapFileMeta);
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut section = String::new();
    let mut first_object = i32::MAX;
    let mut last_object = 0;
    let mut circles = 0;
    let mut sliders = 0;
    let mut spinners = 0;
    let mut holds = 0;
    let mut timing_points: Vec<(f64, f64)> = Vec::new();
    let mut timing_spans: Vec<TimingPointSpan> = Vec::new();
    let mut breaks: Vec<BreakSpan> = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        match section.as_str() {
            "General" => {
                // The only `[General]` key rtosu consumes today. `Mode:` sits in
                // the same section and is what `A-08`/`F-05` need; it is left
                // alone here so this change stays scoped to the SC payload.
                if let Some((key, value)) = line.split_once(':')
                    && key.trim() == "PreviewTime"
                    && let Ok(preview) = value.trim().parse::<i32>()
                {
                    snapshot.preview_time = Some(preview);
                }
            }
            "Metadata" => {
                if let Some((key, value)) = line.split_once(':') {
                    match key.trim() {
                        "Source" => snapshot.source = value.trim().to_string(),
                        "Tags" => snapshot.tags = value.trim().to_string(),
                        _ => {}
                    }
                }
            }
            "Events" => {
                // Two break spellings, and the legacy one is not rare -- map
                // 2964306 (a 6.06-star Extra) uses it. Measured against tosu:
                //
                //   modern   `Break,<start>,<end>,<breakTimeFlag>`
                //   legacy   `2,<start>,<end>`            (LegacyEventType.Break)
                //
                // The legacy form carries no flag, and lazer reports
                // `HasEffect` as true for it, so it defaults to true rather than
                // false. Reading only the modern form silently reported **zero**
                // breaks for a map tosu reported three for.
                let values = line.split(',').map(str::trim).collect::<Vec<_>>();
                let (start_index, has_effect) = match values.first().copied() {
                    Some(first) if first.eq_ignore_ascii_case("break") => (1, None),
                    // A leading `2` is the legacy type code. Only that one value
                    // is a break; `0` is a background/video and `1` a storyboard
                    // layer, so neither may be mistaken for one.
                    Some("2") => (1, Some(true)),
                    _ => continue,
                };
                let (Some(start), Some(end)) = (
                    values.get(start_index).and_then(|v| v.parse::<f64>().ok()),
                    values
                        .get(start_index + 1)
                        .and_then(|v| v.parse::<f64>().ok()),
                ) else {
                    continue;
                };
                breaks.push(BreakSpan {
                    start_time: start.round() as i32,
                    end_time: end.round() as i32,
                    has_effect: has_effect.unwrap_or_else(|| {
                        values.get(start_index + 2).is_some_and(|flag| *flag != "0")
                    }),
                });
            }
            "TimingPoints" => {
                let values = line.split(',').map(str::trim).collect::<Vec<_>>();
                if values.len() >= 2 {
                    if let (Ok(time), Ok(beat_length)) =
                        (values[0].parse::<f64>(), values[1].parse::<f64>())
                    {
                        let bpm = if beat_length >= 0.0 {
                            60_000.0 / beat_length
                        } else {
                            timing_points.last().map(|(_, bpm)| *bpm).unwrap_or(120.0)
                        };
                        timing_points.push((time, bpm));
                        timing_spans.push(TimingPointSpan {
                            time: time.round() as i32,
                            beat_length,
                        });
                    }
                }
            }
            "HitObjects" => {
                let values = line.split(',').map(str::trim).collect::<Vec<_>>();
                if values.len() < 4 {
                    continue;
                }
                let Ok(time) = values[2].parse::<f64>() else {
                    continue;
                };
                // The type has to parse before the object is allowed to widen
                // the map's time range. Reading it afterwards with a fallback of
                // 0 let a malformed line contribute to first_object and
                // last_object while counting towards no object type, so the
                // reported length was set by a line the reader could not
                // actually identify.
                let Ok(kind) = values[3].parse::<u32>() else {
                    continue;
                };
                let time = time.round() as i32;
                first_object = first_object.min(time);
                last_object = last_object.max(time);
                if kind & 128 != 0 {
                    holds += 1;
                } else if kind & 8 != 0 {
                    spinners += 1;
                } else if kind & 2 != 0 {
                    sliders += 1;
                } else if kind & 1 != 0 {
                    circles += 1;
                }
            }
            _ => {}
        }
    }
    if first_object != i32::MAX {
        snapshot.time.first_object = first_object;
        snapshot.time.last_object = last_object;
        snapshot.stats.objects.circles = circles;
        snapshot.stats.objects.sliders = sliders;
        snapshot.stats.objects.spinners = spinners;
        snapshot.stats.objects.holds = holds;
        snapshot.stats.objects.total = circles + sliders + spinners + holds;
    }
    // Assigned unconditionally: a second call on a different file must not leave
    // the previous map's breaks or timing points behind, and an absent line has
    // to overwrite rather than survive.
    snapshot.breaks = breaks;
    snapshot.timing_points = timing_spans;
    if !timing_points.is_empty() {
        let mut min_bpm = f32::MAX;
        let mut max_bpm: f32 = 0.0;
        let mut weighted_bpm = 0.0;
        let mut total_weight = 0.0;
        let end = if last_object > 0 {
            last_object as f64
        } else {
            0.0
        };
        for (index, (time, bpm)) in timing_points.iter().enumerate() {
            if *bpm <= 0.0 {
                continue;
            }
            let next = timing_points
                .get(index + 1)
                .map(|(next, _)| *next)
                .unwrap_or(end.max(*time + 1.0));
            let weight = (next - *time).max(0.0);
            min_bpm = min_bpm.min(*bpm as f32);
            max_bpm = max_bpm.max(*bpm as f32);
            weighted_bpm += *bpm * weight;
            total_weight += weight;
        }
        if min_bpm != f32::MAX && snapshot.stats.bpm.min == 0.0 {
            snapshot.stats.bpm.min = min_bpm;
            snapshot.stats.bpm_base.min = min_bpm;
        }
        if max_bpm != 0.0 && snapshot.stats.bpm.max == 0.0 {
            snapshot.stats.bpm.max = max_bpm;
            snapshot.stats.bpm_base.max = max_bpm;
        }
        if snapshot.stats.bpm.common == 0.0 {
            snapshot.stats.bpm.common = (weighted_bpm / total_weight.max(1.0)) as f32;
        }
        if snapshot.stats.bpm.realtime == 0.0 {
            let realtime = timing_points
                .iter()
                .rev()
                .find(|(time, bpm)| *time <= snapshot.time.live as f64 && *bpm > 0.0)
                .map(|(_, bpm)| *bpm as f32)
                .unwrap_or(snapshot.stats.bpm.common);
            snapshot.stats.bpm.realtime = realtime;
            snapshot.stats.bpm_base.realtime = realtime;
        }
    }
    true
}

/// Round to `decimals` places, matching tosu's `fixDecimals`
/// (`tosu-sourcecode/packages/tosu/src/utils/converters.ts:19-20`).
///
/// Not gated on `pp` any more: it is a pure numeric helper, and the SC payload
/// needs the same rounding without pulling the calculator in. Behaviour is
/// unchanged -- a non-finite input still propagates, and the SC builder folds
/// that to zero itself because tosu's `x || 0` does.
pub fn round_value(value: f32, decimals: u32) -> f32 {
    let factor = 10_f32.powi(decimals as i32);
    (value * factor).round() / factor
}

#[cfg(feature = "pp")]
pub fn populate_beatmap_statistics(
    snapshot: &mut BeatmapSnapshot,
    map: &rosu_pp::Beatmap,
    mods: u32,
) {
    crate::instr_scope!(BeatmapDifficulty);
    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
    populate_beatmap_statistics_with_diff(snapshot, map, &diff, mods);
}

/// Fill the difficulty-derived statistics onto an already-read snapshot.
///
/// Safe to call repeatedly on the same snapshot, which both sessions do on every
/// mod change: every clock-rate-scaled value is derived from the unscaled
/// [`BpmBase`] (falling back to the rosu beatmap's own BPM when the `.osu` was
/// never parsed) rather than from whatever the previous call left behind. A
/// DT then nomod round trip therefore returns the BPM to its base instead of
/// stranding it at the 1.5x it was last scaled to.
///
/// # The per-mode `stars` breakdown
///
/// The breakdown arm matches on the [`rosu_pp::any::DifficultyAttributes`] the
/// calculation already produced for `stars.total` and `max_combo`, and copies
/// the skill values out of it. The source is rosu-pp, never the game: osu!
/// **stable** memory holds none of these values, and the `BeatmapDifficulty`
/// object that tosu v2's own `stats` breakdown is built from comes from osu!
/// lazer, which is not reachable from this process either. So these values match
/// the *shape* of tosu's payload and make no claim to its numbers.
///
/// osu!standard, unchanged: `aim`, `speed`, `slider_factor` and `reading` are
/// rosu-pp's, `flashlight` is dropped when it is zero so that a non-FL map does
/// not report a flashlight, and the miss window is the osu!stable `400.0`
/// constant divided by the clock rate.
///
/// osu!taiko, from [`rosu_pp::taiko::TaikoDifficultyAttributes`]:
///
/// * `stamina`, `rhythm` and `color` are taiko's three rosu-pp skills, wrapped
///   in `Some` so the `Option::is_none` serializer guard keeps emitting them;
///   `reading` is its fourth and is a plain `f32`.
/// * `stars.hit_window` and `hit_window.great` are both
///   `great_hit_window`; `hit_window.ok` is `ok_hit_window`. Both are documented
///   upstream as *already* inclusive of rate-adjusting mods, so unlike the
///   osu!standard miss constant they are deliberately **not** divided by the
///   clock rate a second time.
/// * `flashlight` stays `None`: taiko has no flashlight.
/// * `slider_factor` stays `0.0`: it is an osu!standard-only concept, since
///   taiko converts every slider to a plain note.
/// * `hit_window.meh` and `hit_window.miss` stay `0.0`. osu!taiko has neither a
///   meh nor a miss judgement, and `TaikoDifficultyAttributes` carries no
///   equivalent to them, so reusing the osu!standard `400.0 / clock_rate` miss
///   constant would be inventing a number rather than reporting one.
///
/// osu!catch, osu!mania and osu!criterion keep the whole default breakdown.
/// This is a dependency limitation, not an oversight:
/// [`rosu_pp::catch::CatchDifficultyAttributes`] and
/// [`rosu_pp::mania::ManiaDifficultyAttributes`] expose no skill values and no
/// hit windows whatsoever -- only `stars`, object counts, `max_combo` and
/// `is_convert` -- so there is nothing to copy into `stamina`, `rhythm`, `color`,
/// `reading` or the four hit windows. Mapping osu!standard numbers onto them
/// would report difficulty the calculation never produced, which is worse than
/// a zero that reads as "not available".
#[cfg(feature = "pp")]
pub fn populate_beatmap_statistics_with_diff(
    snapshot: &mut BeatmapSnapshot,
    map: &rosu_pp::Beatmap,
    diff: &rosu_pp::any::DifficultyAttributes,
    mods: u32,
) {
    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let mut circles = 0;
    let mut sliders = 0;
    let mut spinners = 0;
    let mut holds = 0;
    for object in &map.hit_objects {
        if object.is_circle() {
            circles += 1;
        } else if object.is_slider() {
            sliders += 1;
        } else if object.is_spinner() {
            spinners += 1;
        } else if object.is_hold_note() {
            holds += 1;
        }
    }
    snapshot.stats.objects.circles = circles;
    snapshot.stats.objects.sliders = sliders;
    snapshot.stats.objects.spinners = spinners;
    snapshot.stats.objects.holds = holds;
    snapshot.stats.objects.total = map.hit_objects.len() as i32;
    if let Some(last_object) = map.hit_objects.last() {
        let end_time = match &last_object.kind {
            rosu_pp::model::hit_object::HitObjectKind::Slider(slider) => {
                let span_count = slider.span_count() as f64;
                let beat_len = map
                    .timing_points
                    .iter()
                    .rev()
                    .find(|tp| tp.time <= last_object.start_time)
                    .map_or(60_000.0 / 120.0, |tp| tp.beat_len);
                let slider_velocity = map
                    .difficulty_points
                    .iter()
                    .rev()
                    .find(|dp| dp.time <= last_object.start_time)
                    .map_or(1.0, |dp| dp.slider_velocity);
                let dist = slider.expected_dist.unwrap_or(0.0);
                let velocity = 100.0 * map.slider_multiplier * slider_velocity / beat_len;
                let duration = if velocity > 0.0 {
                    (span_count * dist / velocity).round() as i32
                } else {
                    0
                };
                last_object.start_time as i32 + duration
            }
            rosu_pp::model::hit_object::HitObjectKind::Spinner(spinner) => {
                last_object.start_time as i32 + spinner.duration as i32
            }
            rosu_pp::model::hit_object::HitObjectKind::Hold(hold) => {
                last_object.start_time as i32 + hold.duration as i32
            }
            _ => last_object.start_time as i32,
        };
        snapshot.time.last_object = end_time;
    }
    snapshot.stats.max_combo = diff.max_combo() as i32;
    let clock_rate: f32 = if (mods & mod_bits::DT) != 0 || (mods & mod_bits::NC) != 0 {
        1.5
    } else if (mods & mod_bits::HT) != 0 {
        0.75
    } else {
        1.0
    };
    let map_bpm = map.bpm() as f32;
    snapshot.stats.bpm.common = round_value(map_bpm * clock_rate, 2);
    let base = &snapshot.stats.bpm_base;
    snapshot.stats.bpm.realtime = scaled_bpm(base.realtime, map_bpm, clock_rate);
    snapshot.stats.bpm.min = scaled_bpm(base.min, map_bpm, clock_rate);
    snapshot.stats.bpm.max = scaled_bpm(base.max, map_bpm, clock_rate);
    snapshot.stats.stars.total = round_value(diff.stars() as f32, 2);
    snapshot.stats.stars.live = snapshot.stats.stars.total;
    snapshot.stats.ar.original = map.ar;
    snapshot.stats.cs.original = map.cs;
    snapshot.stats.od.original = map.od;
    snapshot.stats.hp.original = map.hp;
    snapshot.stats.ar.converted = round_value(calculate_converted_ar(map.ar, mods, clock_rate), 2);
    snapshot.stats.cs.converted = round_value(calculate_converted_cs(map.cs, mods), 2);
    snapshot.stats.od.converted = round_value(
        calculate_converted_od(map.od, mods, clock_rate, map.mode as u8),
        2,
    );
    snapshot.stats.hp.converted = round_value(calculate_converted_hp(map.hp, mods), 2);
    match diff {
        rosu_pp::any::DifficultyAttributes::Osu(osu_diff) => {
            snapshot.stats.stars.aim = round_value(osu_diff.aim as f32, 2);
            snapshot.stats.stars.speed = round_value(osu_diff.speed as f32, 2);
            snapshot.stats.stars.slider_factor = round_value(osu_diff.slider_factor as f32, 2);
            snapshot.stats.stars.flashlight =
                (osu_diff.flashlight > 0.0).then(|| round_value(osu_diff.flashlight as f32, 2));
            snapshot.stats.stars.reading = round_value(osu_diff.reading as f32, 2);
            snapshot.stats.stars.hit_window = round_value(osu_diff.great_hit_window as f32, 2);
            snapshot.stats.hit_window = HitWindowState {
                miss: 400.0 / (clock_rate as f64),
                meh: osu_diff.meh_hit_window,
                ok: osu_diff.ok_hit_window,
                great: osu_diff.great_hit_window,
            };
        }
        rosu_pp::any::DifficultyAttributes::Taiko(taiko_diff) => {
            snapshot.stats.stars.stamina = Some(round_value(taiko_diff.stamina as f32, 2));
            snapshot.stats.stars.rhythm = Some(round_value(taiko_diff.rhythm as f32, 2));
            snapshot.stats.stars.color = Some(round_value(taiko_diff.color as f32, 2));
            snapshot.stats.stars.reading = round_value(taiko_diff.reading as f32, 2);
            snapshot.stats.stars.hit_window = round_value(taiko_diff.great_hit_window as f32, 2);
            snapshot.stats.hit_window = HitWindowState {
                miss: 0.0,
                meh: 0.0,
                ok: taiko_diff.ok_hit_window,
                great: taiko_diff.great_hit_window,
            };
        }
        _ => {}
    }
    snapshot.stats.pp.ss = crate::pp::calculator::calc_fc_pp(diff, mods_legacy);
    snapshot.stats.pp.fc = snapshot.stats.pp.ss;
}

/// Scale a BPM parsed out of the `.osu` by the clock rate.
///
/// A zero `base` means the `.osu` was never parsed, so the beatmap's own BPM
/// stands in. The fallback keys off the base rather than the last written value,
/// which is what keeps a repeat call idempotent.
#[cfg(feature = "pp")]
fn scaled_bpm(base: f32, map_bpm: f32, clock_rate: f32) -> f32 {
    let base = if base == 0.0 { map_bpm } else { base };
    round_value(base * clock_rate, 2)
}

pub fn calculate_converted_ar(base_ar: f32, mods: u32, clock_rate: f32) -> f32 {
    let mut ar = base_ar;
    if (mods & mod_bits::HR) != 0 {
        ar = (ar * 1.4).min(10.0);
    } else if (mods & mod_bits::EZ) != 0 {
        ar *= 0.5;
    }
    let ms = if ar <= 5.0 {
        1800.0 - 120.0 * ar
    } else {
        1200.0 - 150.0 * (ar - 5.0)
    };
    let scaled_ms = ms / clock_rate;
    if scaled_ms > 1200.0 {
        (1800.0 - scaled_ms) / 120.0
    } else {
        (1200.0 - scaled_ms) / 150.0 + 5.0
    }
}

pub fn calculate_converted_od(base_od: f32, mods: u32, clock_rate: f32, mode: u8) -> f32 {
    let mut od = base_od;
    if (mods & mod_bits::HR) != 0 {
        od = (od * 1.4).min(10.0);
    } else if (mods & mod_bits::EZ) != 0 {
        od *= 0.5;
    }
    if (clock_rate - 1.0).abs() < 1e-5 {
        return od;
    }
    match mode {
        0 => {
            let ms = 80.0 - 6.0 * od;
            let scaled_ms = ms / clock_rate;
            (80.0 - scaled_ms) / 6.0
        }
        1 => {
            let ms = 50.0 - 3.0 * od;
            let scaled_ms = ms / clock_rate;
            (50.0 - scaled_ms) / 3.0
        }
        _ => od,
    }
}

pub fn calculate_converted_cs(base_cs: f32, mods: u32) -> f32 {
    let mut cs = base_cs;
    if (mods & mod_bits::HR) != 0 {
        cs = (cs * 1.3).min(10.0);
    } else if (mods & mod_bits::EZ) != 0 {
        cs *= 0.5;
    }
    cs
}

pub fn calculate_converted_hp(base_hp: f32, mods: u32) -> f32 {
    let mut hp = base_hp;
    if (mods & mod_bits::HR) != 0 {
        hp = (hp * 1.4).min(10.0);
    } else if (mods & mod_bits::EZ) != 0 {
        hp *= 0.5;
    }
    hp
}

/// Read a .NET UTF-16 String from memory at offset from base
pub fn read_net_string(
    memory: &ProcessMemory,
    base: u64,
    offset: i64,
    _pointer_width: usize,
) -> Result<String> {
    let str_ptr_addr = checked_add_signed(base, offset)?;
    let str_ptr = memory.read_pointer(str_ptr_addr)?;
    read_sharp_string_ptr(memory, str_ptr)
}

/// Read a .NET UTF-16 String directly from a String object pointer
pub fn read_sharp_string_ptr(memory: &ProcessMemory, str_ptr: u64) -> Result<String> {
    if str_ptr == 0 {
        return Ok(String::new());
    }
    memory.read_dotnet_string(str_ptr, 2048)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_names() {
        assert_eq!(beatmap_status_name(4), "ranked");
        assert_eq!(beatmap_status_name(7), "loved");
        assert_eq!(beatmap_status_name(1), "notSubmitted");
        assert_eq!(beatmap_status_name(999), "unknown");
    }

    #[test]
    fn test_beatmap_serialization() {
        let mut snapshot = BeatmapSnapshot::default();
        snapshot.id = 12345;
        snapshot.title = "Test Map".to_string();
        snapshot.stats.pp.ss = 512.4;
        snapshot.stats.pp.fc = 512.4;
        snapshot.stats.hit_window = HitWindowState {
            miss: 400.0,
            meh: 100.0,
            ok: 70.0,
            great: 55.5,
        };

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        assert!(json.contains("\"id\":12345"));
        assert!(json.contains("\"title\":\"Test Map\""));
        assert!(json.contains("\"great\":55.5"));
        assert!(!json.contains("\"pp\""));
    }

    #[test]
    #[cfg(feature = "pp")]
    fn test_ar_od_conversion() {
        // HT: 0.75 clock rate
        let ar_ht = round_value(calculate_converted_ar(10.0, 256, 0.75), 2);
        assert_eq!(ar_ht, 9.0);
        let od_ht = round_value(calculate_converted_od(9.0, 256, 0.75, 0), 2);
        assert_eq!(od_ht, 7.56);
        // DT: 1.5 clock rate
        let ar_dt = round_value(calculate_converted_ar(9.0, 64, 1.5), 2);
        assert_eq!(ar_dt, 10.33);

        let test_map = rosu_pp::Beatmap::from_bytes(
            b"osu file format v14\n[TimingPoints]\n0,500,4,1,0,100,1,1\n",
        )
        .unwrap();
        let kiai = test_map
            .effect_points
            .iter()
            .rev()
            .find(|ep| ep.time <= 10.0)
            .map_or(false, |ep| ep.kiai);
        assert!(kiai);
        let is_break = test_map
            .breaks
            .iter()
            .any(|b| 10.0 >= b.start_time && 10.0 <= b.end_time);
        assert!(!is_break);
    }

    #[test]
    fn refresh_needed_when_pointer_changed_with_same_id() {
        assert!(beatmap_refresh_needed(0x2000, 0x1000, 7, 7));
    }

    #[test]
    fn refresh_needed_when_reused_pointer_has_new_id() {
        assert!(beatmap_refresh_needed(0x1000, 0x1000, 8, 7));
    }

    #[test]
    fn refresh_needed_when_pointer_and_id_both_changed() {
        assert!(beatmap_refresh_needed(0x2000, 0x1000, 8, 7));
    }

    #[test]
    fn no_refresh_when_pointer_and_id_both_stable() {
        assert!(!beatmap_refresh_needed(0x1000, 0x1000, 7, 7));
    }

    #[test]
    fn no_refresh_while_id_is_unresolved() {
        assert!(!beatmap_refresh_needed(0x1000, 0x1000, 0, 7));
    }

    #[test]
    fn no_refresh_for_negative_live_id() {
        assert!(!beatmap_refresh_needed(0x1000, 0x1000, -1, 7));
    }

    #[test]
    fn refresh_needed_on_first_resolution() {
        assert!(beatmap_refresh_needed(0x1000, 0x1000, 7, 0));
    }

    /// Two timing points -- 120 BPM from 0 ms, 240 BPM from 2000 ms -- with the
    /// last object at 3000 ms.
    ///
    /// The uneven halves are the point. The rosu beatmap's `bpm()` is the most
    /// *common* beat length, so it reports 120, while the `.osu` parser reports
    /// min 120, max 240 and a 160 weighted average. A fixture with one constant
    /// BPM cannot tell the min/max/realtime bases apart from each other, and
    /// therefore cannot catch a base that is being scaled twice.
    #[cfg(feature = "pp")]
    const BPM_FIXTURE: &[u8] = b"osu file format v14\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n2000,250,4,1,0\n\n[HitObjects]\n0,0,0,1,0,0:0:0:0:\n250,0,1000,1,0,0:0:0:0:\n250,0,2000,1,0,0:0:0:0:\n250,0,3000,1,0,0:0:0:0:\n";

    #[cfg(feature = "pp")]
    fn bpm_fixture_diff(map: &rosu_pp::Beatmap, mods: u32) -> rosu_pp::any::DifficultyAttributes {
        rosu_pp::Difficulty::new()
            .mods(crate::pp::calculator::parse_mods_bits(mods))
            .calculate(map)
    }

    /// A snapshot that has been through [`populate_beatmap_file_metadata`], which
    /// is the state both sessions hand to
    /// [`populate_beatmap_statistics_with_diff`] once a map is resolved. The tag
    /// keeps the parallel test threads off each other's fixture file.
    #[cfg(feature = "pp")]
    fn bpm_fixture_snapshot(tag: &str) -> BeatmapSnapshot {
        let dir = std::env::temp_dir().join(format!("rtosu-bpm-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("fixture.osu");
        std::fs::write(&path, BPM_FIXTURE).expect("write bpm fixture");
        let mut snapshot = BeatmapSnapshot::default();
        assert!(populate_beatmap_file_metadata(&mut snapshot, &path));
        assert_eq!(snapshot.stats.bpm_base.min, 120.0);
        assert_eq!(snapshot.stats.bpm_base.realtime, 120.0);
        assert_eq!(snapshot.stats.bpm_base.max, 240.0);
        snapshot
    }

    /// The FIX-002 regression. Both sessions re-run the statistics on every mod
    /// change, and the old code scaled `realtime`/`min`/`max` in place, so a
    /// second DT call turned 180 into 270 (2.25x overall) instead of leaving it
    /// at 1.5x. `common` comes from the rosu beatmap's 120 BPM, so DT reads 180.
    #[test]
    #[cfg(feature = "pp")]
    fn double_time_scales_the_bpm_once_however_many_times_the_stats_run() {
        let map = rosu_pp::Beatmap::from_bytes(BPM_FIXTURE).expect("parse bpm fixture");
        assert_eq!(map.bpm(), 120.0);
        let mut snapshot = bpm_fixture_snapshot("dt");

        for call in 1..=2 {
            populate_beatmap_statistics_with_diff(
                &mut snapshot,
                &map,
                &bpm_fixture_diff(&map, mod_bits::DT),
                mod_bits::DT,
            );
            assert_eq!(snapshot.stats.bpm.common, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.realtime, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.min, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.max, 360.0, "call {call}");
        }

        assert_ne!(snapshot.stats.bpm.realtime, 270.0);
        assert_ne!(snapshot.stats.bpm.max, 540.0);
    }

    /// The other half of the same bug, and the one a double-call test cannot
    /// see: dropping back to nomod used to multiply by 1.0, which left the BPM
    /// stranded at the 1.5x it was last scaled to. Every field has to return to
    /// its own parsed base, with `max` still the 240 BPM half of the map.
    #[test]
    #[cfg(feature = "pp")]
    fn leaving_double_time_restores_the_parsed_bpm() {
        let map = rosu_pp::Beatmap::from_bytes(BPM_FIXTURE).expect("parse bpm fixture");
        let mut snapshot = bpm_fixture_snapshot("dt-nomod");
        populate_beatmap_statistics_with_diff(
            &mut snapshot,
            &map,
            &bpm_fixture_diff(&map, mod_bits::DT),
            mod_bits::DT,
        );

        populate_beatmap_statistics_with_diff(&mut snapshot, &map, &bpm_fixture_diff(&map, 0), 0);

        assert_eq!(snapshot.stats.bpm.common, 120.0);
        assert_eq!(snapshot.stats.bpm.realtime, 120.0);
        assert_eq!(snapshot.stats.bpm.min, 120.0);
        assert_eq!(snapshot.stats.bpm.max, 240.0);
    }

    /// Half Time is the same idempotence at 0.75x: 120 becomes 90 and the 240
    /// maximum becomes 180, on the first call and on the repeat.
    #[test]
    #[cfg(feature = "pp")]
    fn half_time_scales_the_bpm_once_however_many_times_the_stats_run() {
        let map = rosu_pp::Beatmap::from_bytes(BPM_FIXTURE).expect("parse bpm fixture");
        let mut snapshot = bpm_fixture_snapshot("ht");

        for call in 1..=2 {
            populate_beatmap_statistics_with_diff(
                &mut snapshot,
                &map,
                &bpm_fixture_diff(&map, mod_bits::HT),
                mod_bits::HT,
            );
            assert_eq!(snapshot.stats.bpm.common, 90.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.realtime, 90.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.min, 90.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.max, 180.0, "call {call}");
        }
    }

    /// A snapshot whose `.osu` was never parsed has no bases at all, so all
    /// three fall back to the rosu beatmap's own 120 BPM. The fallback keys off
    /// the base rather than the last written value, so it stays idempotent.
    #[test]
    #[cfg(feature = "pp")]
    fn a_snapshot_without_file_metadata_falls_back_and_stays_idempotent() {
        let map = rosu_pp::Beatmap::from_bytes(BPM_FIXTURE).expect("parse bpm fixture");
        let mut snapshot = BeatmapSnapshot::default();
        assert_eq!(snapshot.stats.bpm_base, BpmBase::default());

        for call in 1..=2 {
            populate_beatmap_statistics_with_diff(
                &mut snapshot,
                &map,
                &bpm_fixture_diff(&map, mod_bits::DT),
                mod_bits::DT,
            );
            assert_eq!(snapshot.stats.bpm.common, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.realtime, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.min, 180.0, "call {call}");
            assert_eq!(snapshot.stats.bpm.max, 180.0, "call {call}");
        }
    }

    /// The bases are internal bookkeeping, not part of the tosu v2 payload: the
    /// emitted `bpm` object stays exactly the four documented fields, and `pp`
    /// stays out of it too.
    #[test]
    fn the_parsed_bpm_bases_stay_out_of_the_beatmap_json() {
        let mut snapshot = BeatmapSnapshot::default();
        snapshot.stats.bpm.realtime = 180.0;
        snapshot.stats.bpm.common = 180.0;
        snapshot.stats.bpm.min = 180.0;
        snapshot.stats.bpm.max = 360.0;
        snapshot.stats.bpm_base.realtime = 120.0;
        snapshot.stats.bpm_base.min = 120.0;
        snapshot.stats.bpm_base.max = 240.0;

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        assert!(!json.contains("\"pp\""));
        assert!(!json.contains("bpmBase"));
        assert!(!json.contains("base"));
        assert!(
            json.contains(
                "\"bpm\":{\"realtime\":180.0,\"common\":180.0,\"min\":180.0,\"max\":360.0}"
            )
        );
    }

    /// A 300 BPM alternating stream of 24 hit circles, timed 0-3800 ms, with a
    /// 1/8 note dropped into every other bar so the rhythm evaluator has a
    /// pattern that is not a flat quarter-note grid.
    ///
    /// The hit sound alternates `0` (normal) and `8` (clap), and that is the
    /// point: rosu derives a taiko note's inner/outer type from the `CLAP` or
    /// `WHISTLE` sound flag, not from the `x` position, so an all-`0` fixture
    /// would be a single-colour stream and would report `color: 0`. Alternating
    /// the two gives the colour evaluator real colour changes.
    ///
    /// The same bytes are reused for `Mode: 2` and `Mode: 3`. Hit circles are
    /// valid in every mode, so one fixture can drive the taiko, osu!catch,
    /// osu!mania and osu!standard arms with only the `Mode:` line differing.
    #[cfg(feature = "pp")]
    const MODE_FIXTURE_OBJECTS: &str = "\
0,192,0,1,0,0:0:0:0:
192,320,200,1,8,0:0:0:0:
0,192,400,1,0,0:0:0:0:
192,320,600,1,8,0:0:0:0:
0,192,800,1,0,0:0:0:0:
192,320,1000,1,8,0:0:0:0:
0,192,1100,1,0,0:0:0:0:
192,320,1200,1,8,0:0:0:0:
0,192,1400,1,0,0:0:0:0:
192,320,1600,1,8,0:0:0:0:
0,192,1800,1,0,0:0:0:0:
192,320,1900,1,8,0:0:0:0:
0,192,2000,1,0,0:0:0:0:
192,320,2200,1,8,0:0:0:0:
0,192,2400,1,0,0:0:0:0:
192,320,2500,1,8,0:0:0:0:
0,192,2600,1,0,0:0:0:0:
192,320,2800,1,8,0:0:0:0:
0,192,3000,1,0,0:0:0:0:
192,320,3200,1,8,0:0:0:0:
0,192,3400,1,0,0:0:0:0:
192,320,3500,1,8,0:0:0:0:
0,192,3600,1,0,0:0:0:0:
192,320,3800,1,8,0:0:0:0:
";

    /// The [`MODE_FIXTURE_OBJECTS`] stream behind a `[General] Mode:` line.
    ///
    /// `Mode: 0` is osu!standard, `1` osu!taiko, `2` osu!catch and `3`
    /// osu!mania. Everything else is identical on purpose, so any difference in
    /// the emitted breakdown is caused by the mode and by nothing else.
    #[cfg(feature = "pp")]
    fn mode_fixture(mode: u8) -> String {
        format!(
            "osu file format v14\n\n[General]\nMode:{mode}\n\n[Difficulty]\nHPDrainRate:6\nCircleSize:5\nOverallDifficulty:7\nApproachRate:8\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,200,4,1,0\n\n[HitObjects]\n{MODE_FIXTURE_OBJECTS}"
        )
    }

    /// The rosu beatmap and the attributes the arm under test will see for
    /// `mode`, with no mods applied.
    #[cfg(feature = "pp")]
    fn mode_fixture_diff(mode: u8) -> (rosu_pp::Beatmap, rosu_pp::any::DifficultyAttributes) {
        let map = rosu_pp::Beatmap::from_bytes(mode_fixture(mode).as_bytes())
            .unwrap_or_else(|err| panic!("parse mode {mode} fixture: {err}"));
        let diff = bpm_fixture_diff(&map, 0);
        (map, diff)
    }

    /// FIX-029, osu!taiko. Every skill rosu-pp reports for taiko reaches the
    /// snapshot, including the three that only taiko has: `stamina`, `rhythm`
    /// and `color` are all populated and all above zero on this fixture, and
    /// `reading` is a real value rather than a left-over default.
    #[test]
    #[cfg(feature = "pp")]
    fn taiko_populates_its_own_skills_and_hit_windows() {
        let (map, diff) = mode_fixture_diff(1);
        let mut snapshot = BeatmapSnapshot::default();
        populate_beatmap_statistics_with_diff(&mut snapshot, &map, &diff, 0);

        let stars = &snapshot.stats.stars;
        assert!(stars.stamina.expect("taiko stamina is populated") > 0.0);
        assert!(stars.rhythm.expect("taiko rhythm is populated") > 0.0);
        assert!(stars.color.expect("taiko color is populated") > 0.0);
        assert!(stars.reading > 0.0);
        assert!(stars.hit_window > 0.0);
        assert!(stars.total > 0.0);
        assert!(snapshot.stats.hit_window.great > 0.0);
        assert!(snapshot.stats.hit_window.ok > 0.0);

        assert_eq!(stars.stamina, Some(1.1));
        assert_eq!(stars.rhythm, Some(0.17));
        assert_eq!(stars.color, Some(0.15));
        assert_eq!(stars.reading, 0.14);
        assert_eq!(stars.hit_window, 28.5);
        assert_eq!(stars.total, 1.56);
        assert_eq!(stars.live, stars.total);
        assert_eq!(snapshot.stats.hit_window.great, 28.5);
        assert_eq!(snapshot.stats.hit_window.ok, 67.5);
    }

    /// The values the taiko arm deliberately refuses to invent. `flashlight`
    /// has no taiko equivalent at all, `slider_factor` is an osu!standard-only
    /// concept, and `meh` and `miss` are judgements osu!taiko does not have.
    ///
    /// Each of these is a zero that means "not available", so they are pinned
    /// here: filling any of them in with an osu!standard number would be a
    /// regression that looks like a feature, and nothing else would catch it.
    #[test]
    #[cfg(feature = "pp")]
    fn taiko_leaves_flashlight_and_the_meh_and_miss_windows_alone() {
        let (map, diff) = mode_fixture_diff(1);
        let mut snapshot = BeatmapSnapshot::default();
        populate_beatmap_statistics_with_diff(&mut snapshot, &map, &diff, 0);

        assert_eq!(snapshot.stats.stars.flashlight, None);
        assert_eq!(snapshot.stats.stars.slider_factor, 0.0);
        assert_eq!(
            snapshot.stats.hit_window,
            HitWindowState {
                miss: 0.0,
                meh: 0.0,
                ok: 67.5,
                great: 28.5,
            }
        );

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        assert!(!json.contains("flashlight"));
        assert!(json.contains("\"stamina\":1.1"));
        assert!(json.contains("\"rhythm\":0.17"));
        assert!(json.contains("\"color\":0.15"));
    }

    /// The reason the taiko arm does not divide by the clock rate.
    ///
    /// `great_hit_window` and `ok_hit_window` come out of rosu-pp already
    /// inclusive of rate-adjusting mods, so DT moves the nomod 28.5/67.5 down to
    /// 19.0/45.0 on its own. Dividing again would report 12.67 and 30.0. The
    /// osu!standard arm *does* divide, because its `400.0` miss constant is a
    /// raw osu!stable number rather than a rosu-pp attribute, so the two arms
    /// disagree on purpose and this is what keeps them from being
    /// "reconciled" into one.
    #[test]
    #[cfg(feature = "pp")]
    fn taiko_hit_windows_already_include_the_clock_rate() {
        let map =
            rosu_pp::Beatmap::from_bytes(mode_fixture(1).as_bytes()).expect("parse taiko fixture");
        let mut snapshot = BeatmapSnapshot::default();
        populate_beatmap_statistics_with_diff(
            &mut snapshot,
            &map,
            &bpm_fixture_diff(&map, mod_bits::DT),
            mod_bits::DT,
        );

        assert_eq!(snapshot.stats.hit_window.great, 19.0);
        assert_eq!(snapshot.stats.hit_window.ok, 45.0);
        assert_eq!(snapshot.stats.hit_window.miss, 0.0);
        assert_eq!(snapshot.stats.hit_window.meh, 0.0);
        assert_eq!(snapshot.stats.stars.hit_window, 19.0);
    }

    /// FIX-029, osu!catch and osu!mania. Neither attribute struct carries a
    /// skill value or a hit window, so both breakdowns must stay at the
    /// default -- which is the whole point of leaving those two arms out rather
    /// than copying osu!standard numbers onto them.
    ///
    /// The same bytes in `Mode: 0` *do* fill the osu!standard breakdown, which
    /// is asserted here too: without that contrast, a fixture that parsed badly
    /// would produce the same all-zero result and the test would pass for the
    /// wrong reason.
    #[test]
    #[cfg(feature = "pp")]
    fn catch_and_mania_keep_the_default_breakdown() {
        for mode in [2u8, 3] {
            let (map, diff) = mode_fixture_diff(mode);
            let mut snapshot = BeatmapSnapshot::default();
            populate_beatmap_statistics_with_diff(&mut snapshot, &map, &diff, 0);

            assert!(
                snapshot.stats.stars.total > 0.0,
                "mode {mode} still calculates stars"
            );
            assert_eq!(snapshot.stats.stars.stamina, None, "mode {mode}");
            assert_eq!(snapshot.stats.stars.rhythm, None, "mode {mode}");
            assert_eq!(snapshot.stats.stars.color, None, "mode {mode}");
            assert_eq!(snapshot.stats.stars.flashlight, None, "mode {mode}");
            assert_eq!(snapshot.stats.stars.reading, 0.0, "mode {mode}");
            assert_eq!(snapshot.stats.stars.aim, 0.0, "mode {mode}");
            assert_eq!(snapshot.stats.stars.speed, 0.0, "mode {mode}");
            assert_eq!(snapshot.stats.stars.slider_factor, 0.0, "mode {mode}");
            assert_eq!(snapshot.stats.stars.hit_window, 0.0, "mode {mode}");
            assert_eq!(
                snapshot.stats.hit_window,
                HitWindowState::default(),
                "mode {mode}"
            );

            let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
            assert!(!json.contains("stamina"), "mode {mode}");
            assert!(!json.contains("rhythm"), "mode {mode}");
            assert!(!json.contains("color"), "mode {mode}");
        }

        let (map, diff) = mode_fixture_diff(0);
        let mut snapshot = BeatmapSnapshot::default();
        populate_beatmap_statistics_with_diff(&mut snapshot, &map, &diff, 0);
        assert_eq!(snapshot.stats.stars.aim, 2.16);
        assert_eq!(snapshot.stats.stars.speed, 1.29);
        assert_eq!(snapshot.stats.stars.reading, 1.09);
        assert_eq!(snapshot.stats.stars.hit_window, 37.5);
        assert_eq!(
            snapshot.stats.hit_window,
            HitWindowState {
                miss: 400.0,
                meh: 129.5,
                ok: 83.5,
                great: 37.5,
            }
        );
    }

    /// The `[General]` table of an `.osu` file has no `AudioLength` option, so
    /// this arm could never fire. The song length comes from the game instead,
    /// read off the audio object at `get_audio_length_ptr` in both sessions.
    /// The assertion is on the *behaviour* rather than the removal: a fixture
    /// carrying a bogus `AudioLength` must not reach the payload.
    #[test]
    fn a_file_supplied_audio_length_is_ignored() {
        let snapshot = file_metadata_snapshot(
            "audio-length",
            "osu file format v14\n\n[General]\nAudioLength:12345\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,0,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.time.mp3_length, 0);
    }

    /// Nothing in `[General]` is read any more, so the section has to be inert
    /// rather than break the scan that follows it. Several of these lines carry
    /// colons inside their values and one is a duplicate `Mode`, so this is also
    /// the check that ignoring the section does not disturb the object and
    /// timing parsing further down the file.
    #[test]
    fn a_general_section_is_inert_and_the_scan_continues() {
        let snapshot = file_metadata_snapshot(
            "general",
            "osu file format v14\n\n[General]\nMode:0\nAudioFilename:audio.mp3\nAudioLeadIn:0\nPreviewTime:-1\nCountdown:0\nSampleSet:Normal\nStackLeniency:0.7\nMode:3\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n64,192,2500,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.time.mp3_length, 0);
        assert_eq!(snapshot.time.first_object, 1000);
        assert_eq!(snapshot.time.last_object, 2500);
        assert_eq!(snapshot.stats.objects.circles, 2);
        assert_eq!(snapshot.stats.objects.total, 2);
        // 60_000 / 500, so the timing points were parsed too.
        assert_eq!(snapshot.stats.bpm.min, 120.0);
    }

    /// A hit object whose type does not parse must not be able to widen the
    /// map's reported time range. The trailing object below sits at 9000 ms
    /// with a non-numeric type, so a reader that counted it would report
    /// `last_object = 9000` and stretch the map by 6 seconds.
    #[test]
    fn a_hit_object_with_an_unparseable_type_does_not_widen_the_map() {
        let snapshot = file_metadata_snapshot(
            "bad-type",
            "osu file format v14\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n64,192,2000,1,0,0:0:0:0:\n64,192,9000,not-a-type,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.time.first_object, 1000);
        assert_eq!(snapshot.time.last_object, 2000);
        assert_eq!(snapshot.stats.objects.circles, 2);
        assert_eq!(snapshot.stats.objects.total, 2);
    }

    /// The companion to the test above: a well-formed trailing object still
    /// extends the range. Without this, a fix that simply stopped reading
    /// `last_object` would pass the regression test.
    #[test]
    fn a_well_formed_trailing_object_still_extends_the_map() {
        let snapshot = file_metadata_snapshot(
            "good-type",
            "osu file format v14\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n64,192,9000,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.time.first_object, 1000);
        assert_eq!(snapshot.time.last_object, 9000);
        assert_eq!(snapshot.stats.objects.circles, 2);
    }

    /// Every object type is classified before the range widens, so a spinner or
    /// hold note after a malformed line must still be counted.
    #[test]
    fn object_types_after_a_malformed_line_are_still_counted() {
        let snapshot = file_metadata_snapshot(
            "mixed-types",
            "osu file format v14\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,1000,2,0,L|400:200,1,140\n64,192,2000,12,0,9000\n64,192,9000,bogus,0,0:0:0:0:\n64,192,3000,128,0,3500:3000:0:0:0:\n",
        );
        let objects = &snapshot.stats.objects;
        assert_eq!(objects.sliders, 1);
        assert_eq!(objects.spinners, 1);
        assert_eq!(objects.holds, 1);
        assert_eq!(objects.circles, 0);
        assert_eq!(objects.total, 3);
        assert_eq!(snapshot.time.first_object, 1000);
        assert_eq!(snapshot.time.last_object, 3000);
    }

    /// Run `populate_beatmap_file_metadata` over a fixture written to a temporary
    /// path. The tag keeps the parallel test threads off each other's file.
    fn file_metadata_snapshot(tag: &str, contents: &str) -> BeatmapSnapshot {
        let dir = std::env::temp_dir().join(format!("rtosu-filemeta-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("fixture.osu");
        std::fs::write(&path, contents).expect("write fixture");
        let mut snapshot = BeatmapSnapshot::default();
        assert!(populate_beatmap_file_metadata(&mut snapshot, &path));
        snapshot
    }

    /// `[General] PreviewTime` is the source of SC's `previewtime` leaf
    /// (`buildResultSC.ts:116` <- `states/beatmap.ts:488`). The section used to
    /// be skipped whole, which is why the leaf had no source.
    #[test]
    fn the_general_section_now_yields_the_preview_time() {
        let snapshot = file_metadata_snapshot(
            "preview",
            "osu file format v14\n\n[General]\nMode:0\nPreviewTime:72834\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,300,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.preview_time, Some(72834));
    }

    /// osu!lazer reports `-1` for a map with no `PreviewTime` line, and tosu
    /// forwards that, so an absent line must not read as `0` -- and an explicit
    /// `PreviewTime:-1` has to survive as the same value rather than being
    /// confused with absence.
    #[test]
    fn an_absent_preview_time_stays_unresolved_rather_than_becoming_zero() {
        let with_line = file_metadata_snapshot(
            "preview-neg",
            "osu file format v14\n\n[General]\nPreviewTime:-1\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,300,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(with_line.preview_time, Some(-1));

        let without_line = file_metadata_snapshot(
            "preview-absent",
            "osu file format v14\n\n[General]\nMode:0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,300,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(without_line.preview_time, None);

        // A snapshot built straight from memory never sees the file at all.
        assert_eq!(BeatmapSnapshot::default().preview_time, None);
    }

    /// `[Events] Break` is the source of SC's `mapBreaks`
    /// (`buildResultSC.ts:131-135`). The fourth field is the break-time flag,
    /// which is lazer's `BeatmapBreak.HasEffect`.
    #[test]
    fn the_events_section_yields_break_spans_with_their_effect_flag() {
        let snapshot = file_metadata_snapshot(
            "breaks",
            "osu file format v14\n\n[General]\nMode:0\n\n[Events]\n0,0,0,0,0,0\nBreak,34934,36564,0\nBreak,70000,72000,1\nBackground,,\"bg.jpg\",0,0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,300,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(
            snapshot.breaks.len(),
            2,
            "non-Break event lines are skipped"
        );
        assert_eq!(snapshot.breaks[0].start_time, 34934);
        assert_eq!(snapshot.breaks[0].end_time, 36564);
        assert!(!snapshot.breaks[0].has_effect, "flag 0");
        assert_eq!(snapshot.breaks[1].start_time, 70000);
        assert_eq!(snapshot.breaks[1].end_time, 72000);
        assert!(snapshot.breaks[1].has_effect, "flag 1");
    }

    /// A second call on a different file has to clear the previous map's spans,
    /// or a map with no breaks would report the previous map's.
    #[test]
    fn a_second_file_replaces_the_breaks_and_timing_points() {
        let dir = std::env::temp_dir().join(format!(
            "rtosu-filemeta-{}-breaks-replace",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("fixture.osu");

        let with_breaks = "osu file format v14\n\n[Events]\nBreak,100,200,0\n\n[TimingPoints]\n0,300,4,1,0\n\n[Difficulty]\nCircleSize:4\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n";
        std::fs::write(&path, with_breaks).expect("write first fixture");
        let mut snapshot = BeatmapSnapshot::default();
        assert!(populate_beatmap_file_metadata(&mut snapshot, &path));
        assert_eq!(snapshot.breaks.len(), 1);
        assert_eq!(snapshot.timing_points.len(), 1);

        let without_breaks = "osu file format v14\n\n[Difficulty]\nCircleSize:4\n\n[TimingPoints]\n0,500,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n";
        std::fs::write(&path, without_breaks).expect("write second fixture");
        assert!(populate_beatmap_file_metadata(&mut snapshot, &path));
        assert!(snapshot.breaks.is_empty(), "stale breaks survived");
        assert_eq!(snapshot.timing_points.len(), 1);
        assert_eq!(
            snapshot.timing_points[0].beat_length, 500.0,
            "stale timing point"
        );
    }

    /// SC's `mapTimingPoints[].beatLength` is lazer's raw declared
    /// `TimingChangePoint.beatLength`, so a redline keeps its **negative** value
    /// rather than being normalised to the inherited length.
    #[test]
    fn timing_point_spans_keep_the_raw_declared_beat_length() {
        let snapshot = file_metadata_snapshot(
            "timing",
            "osu file format v14\n\n[General]\nMode:0\n\n[Difficulty]\nCircleSize:4\n\n[TimingPoints]\n0,300,4,1,0\n1000,500,4,1,0\n5000,-150,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.timing_points.len(), 3);
        assert_eq!(snapshot.timing_points[0].time, 0);
        assert_eq!(snapshot.timing_points[0].beat_length, 300.0);
        assert_eq!(snapshot.timing_points[1].time, 1000);
        assert_eq!(snapshot.timing_points[1].beat_length, 500.0);
        assert_eq!(
            snapshot.timing_points[2].beat_length, -150.0,
            "a redline keeps its negative declared length"
        );
    }

    /// The three new fields feed only the SC payload, so the v2 packet that
    /// already ships must not gain a key because of them.
    /// The **legacy** break spelling, from a real map.
    ///
    /// Map 2964306, "Toono Gensou Monogatari (MRM REMIX) (-Syncro) [Extra]", a
    /// 6.06-star Extra, writes its breaks as `2,<start>,<end>` with no
    /// `Break` keyword and no flag. The section below is transcribed verbatim
    /// from that file, and the three expected spans are what tosu 4.26.2
    /// reported for `mapBreaks` against the same map.
    ///
    /// This is a real miss, not a hypothetical one: reading only the modern
    /// spelling reported **zero** breaks for a map tosu reported three for, and
    /// the only way it was caught was the live diff. The legacy form carries no
    /// flag, so `hasEffect` defaults to true -- which is what lazer reports and
    /// what tosu served.
    #[test]
    fn the_legacy_numeric_break_spelling_is_parsed() {
        let snapshot = file_metadata_snapshot(
            "breaks-legacy",
            concat!(
                "osu file format v14\n\n",
                "[General]\nAudioFilename: audio.mp3\nPreviewTime: 72834\nMode: 0\n\n",
                "[Events]\n",
                "//Background and Video events\n",
                "0,0,\"Chen_waifu2x_art_noise1_scale_tta_1 (1).png\",0,0\n",
                "//Break Periods\n",
                "2,34934,36564\n",
                "2,92534,94014\n",
                "2,94934,95664\n",
                "//Storyboard Layer 0 (Background)\n",
                "\n[Difficulty]\nCircleSize:4\n\n[TimingPoints]\n0,300,4,1,0\n\n",
                "[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n"
            ),
        );

        assert_eq!(snapshot.breaks.len(), 3, "one legacy break per `2,` line");
        // Transcribed from tosu's live `mapBreaks` for this map.
        let expected = [
            (34934, 36564, true),
            (92534, 94014, true),
            (94934, 95664, true),
        ];
        for (span, (start, end, has_effect)) in snapshot.breaks.iter().zip(expected) {
            assert_eq!(span.start_time, start);
            assert_eq!(span.end_time, end);
            assert!(span.has_effect, "the legacy form defaults to true");
        }
    }

    /// Only a leading `2` is a break. `0` is a background/video event and `1` a
    /// storyboard layer, and a modern `Background,...` or `Storyboard,...` line
    /// is not a break either -- so neither may be mistaken for one.
    #[test]
    fn background_and_storyboard_events_are_not_breaks() {
        let snapshot = file_metadata_snapshot(
            "breaks-not-breaks",
            concat!(
                "osu file format v14\n\n",
                "[Events]\n",
                "0,0,\"bg.jpg\",0,0\n",
                "1,512,0,0,0,0,1,0,0,0\n",
                "2,0,0,0,0,0\n",
                "Break,100,200,0\n",
                "0,\"sound.mp3\",0,0,0,0,0,0,0,0\n",
                "Background,\"bg2.jpg\",0,0\n",
                "Storyboard Layer 0 (Background)\n",
                "//2,300,400\n",
                "\n[Difficulty]\nCircleSize:4\n\n[TimingPoints]\n0,300,4,1,0\n\n",
                "[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n"
            ),
        );
        assert_eq!(snapshot.breaks.len(), 2, "only the `2,` and `Break,` lines");
        assert_eq!(snapshot.breaks[0].start_time, 0);
        assert_eq!(snapshot.breaks[0].end_time, 0);
        assert_eq!(snapshot.breaks[1].start_time, 100);
        assert_eq!(snapshot.breaks[1].end_time, 200);
    }

    #[test]
    fn the_new_file_fields_stay_out_of_the_serialised_snapshot() {
        let snapshot = file_metadata_snapshot(
            "no-leak",
            "osu file format v14\n\n[General]\nPreviewTime:72834\n\n[Events]\nBreak,100,200,1\n\n[Difficulty]\nCircleSize:4\n\n[TimingPoints]\n0,300,4,1,0\n\n[HitObjects]\n64,192,1134,1,0,0:0:0:0:\n",
        );
        assert_eq!(snapshot.breaks.len(), 1);
        assert_eq!(snapshot.preview_time, Some(72834));

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        for absent in ["\"breaks\"", "\"timingPoints\"", "\"previewTime\""] {
            assert!(!json.contains(absent), "v2 must not gain {absent}");
        }
        // The `#[serde(skip)]` fields still round-trip as their defaults, which
        // is what a deserialised v2 packet carries.
        let parsed: BeatmapSnapshot = serde_json::from_str(&json).expect("round trip");
        assert!(parsed.breaks.is_empty());
        assert!(parsed.timing_points.is_empty());
        assert_eq!(parsed.preview_time, None);
    }
}
