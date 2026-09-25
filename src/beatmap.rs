use crate::address::checked_add_signed;
use crate::process::ProcessMemory;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCounts {
    pub circles: i32,
    pub sliders: i32,
    pub spinners: i32,
    pub holds: i32,
    pub total: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HitWindowState {
    #[serde(flatten)]
    pub values: BTreeMap<String, f32>,
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

/// Read active beatmap metadata from osu! process memory given the base pointer address
pub fn read_beatmap_memory(
    memory: &ProcessMemory,
    base_addr: u64,
    play_time_addr: Option<u64>,
    pointer_width: usize,
) -> Result<BeatmapSnapshot> {
    // In osu! stable: base_addr - 0xc points to beatmap pointer (indirect dereference)
    let beatmap_ptr_addr = checked_add_signed(base_addr, -0xc)?;
    let beatmap_addr = memory
        .read_indirect_pointer(beatmap_ptr_addr)
        .context("reading beatmap pointer")?;

    if beatmap_addr == 0 {
        return Ok(BeatmapSnapshot::default());
    }

    // Read audio playback time if available
    let live_time = if let Some(pt_addr) = play_time_addr {
        let time_ptr_addr = checked_add_signed(pt_addr, 0x5).unwrap_or(pt_addr);
        let time_ptr = memory.read_pointer(time_ptr_addr).unwrap_or(0);
        if time_ptr != 0 {
            memory.read_i32(time_ptr).unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };

    let id = memory
        .read_i32(checked_add_signed(beatmap_addr, 0xC8)?)
        .unwrap_or(0);
    let set_id = memory
        .read_i32(checked_add_signed(beatmap_addr, 0xCC)?)
        .unwrap_or(0);
    let status_num = memory
        .read_i32(checked_add_signed(beatmap_addr, 0x12C)?)
        .unwrap_or(0);
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
        audio_filename,
        background_filename,
        source: String::new(),
        tags: String::new(),
        stats: BeatmapStats {
            stars: StarsBreakdown::default(),
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
            bpm: BpmStats::default(),
            objects: ObjectCounts {
                total: object_count,
                ..ObjectCounts::default()
            },
            hit_window: HitWindowState::default(),
            max_combo: 0,
            pp: BeatmapPpStats::default(),
        },
    })
}

pub fn populate_beatmap_file_metadata(snapshot: &mut BeatmapSnapshot, path: &Path) -> bool {
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
            "Metadata" => {
                if let Some((key, value)) = line.split_once(':') {
                    match key.trim() {
                        "Source" => snapshot.source = value.trim().to_string(),
                        "Tags" => snapshot.tags = value.trim().to_string(),
                        _ => {}
                    }
                }
            }
            "General" => {
                if let Some((key, value)) = line.split_once(':')
                    && key.trim() == "AudioLength"
                {
                    snapshot.time.mp3_length = value.trim().parse().unwrap_or(0);
                }
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
                let time = time.round() as i32;
                first_object = first_object.min(time);
                last_object = last_object.max(time);
                let kind = values[3].parse::<u32>().unwrap_or(0);
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
        }
        if max_bpm != 0.0 && snapshot.stats.bpm.max == 0.0 {
            snapshot.stats.bpm.max = max_bpm;
        }
        if snapshot.stats.bpm.common == 0.0 {
            snapshot.stats.bpm.common = (weighted_bpm / total_weight.max(1.0)) as f32;
        }
        if snapshot.stats.bpm.realtime == 0.0 {
            snapshot.stats.bpm.realtime = timing_points
                .iter()
                .rev()
                .find(|(time, bpm)| *time <= snapshot.time.live as f64 && *bpm > 0.0)
                .map(|(_, bpm)| *bpm as f32)
                .unwrap_or(snapshot.stats.bpm.common);
        }
    }
    true
}

#[cfg(feature = "pp")]
fn round_value(value: f32, decimals: u32) -> f32 {
    let factor = 10_f32.powi(decimals as i32);
    (value * factor).round() / factor
}

#[cfg(feature = "pp")]
pub fn populate_beatmap_statistics(
    snapshot: &mut BeatmapSnapshot,
    map: &rosu_pp::Beatmap,
    mods: u32,
) {
    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
    populate_beatmap_statistics_with_diff(snapshot, map, &diff, mods);
}

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
    let clock_rate: f32 = if (mods & 64) != 0 || (mods & 512) != 0 {
        1.5
    } else if (mods & 256) != 0 {
        0.75
    } else {
        1.0
    };
    let bpm = (map.bpm() as f32) * clock_rate;
    snapshot.stats.bpm.common = round_value(bpm, 4);
    if snapshot.stats.bpm.realtime == 0.0 {
        snapshot.stats.bpm.realtime = round_value(bpm, 4);
    } else {
        snapshot.stats.bpm.realtime = round_value(snapshot.stats.bpm.realtime * clock_rate, 4);
    }
    if snapshot.stats.bpm.min == 0.0 {
        snapshot.stats.bpm.min = round_value(bpm, 4);
    } else {
        snapshot.stats.bpm.min = round_value(snapshot.stats.bpm.min * clock_rate, 4);
    }
    if snapshot.stats.bpm.max == 0.0 {
        snapshot.stats.bpm.max = round_value(bpm, 4);
    } else {
        snapshot.stats.bpm.max = round_value(snapshot.stats.bpm.max * clock_rate, 4);
    }
    snapshot.stats.stars.total = round_value(diff.stars() as f32, 2);
    snapshot.stats.stars.live = snapshot.stats.stars.total;
    if let rosu_pp::any::DifficultyAttributes::Osu(osu_diff) = diff {
        snapshot.stats.stars.aim = round_value(osu_diff.aim as f32, 2);
        snapshot.stats.stars.speed = round_value(osu_diff.speed as f32, 2);
        snapshot.stats.stars.slider_factor = round_value(osu_diff.slider_factor as f32, 2);
        snapshot.stats.stars.flashlight =
            (osu_diff.flashlight > 0.0).then(|| round_value(osu_diff.flashlight as f32, 2));
        snapshot.stats.stars.reading = round_value(osu_diff.reading as f32, 2);
        snapshot.stats.stars.hit_window = round_value(osu_diff.great_hit_window as f32, 2);
        snapshot.stats.hit_window.values.clear();
        snapshot
            .stats
            .hit_window
            .values
            .insert("miss".to_string(), 400.0);
        snapshot.stats.hit_window.values.insert(
            "meh".to_string(),
            round_value(osu_diff.meh_hit_window as f32, 2),
        );
        snapshot.stats.hit_window.values.insert(
            "ok".to_string(),
            round_value(osu_diff.ok_hit_window as f32, 2),
        );
        snapshot.stats.hit_window.values.insert(
            "great".to_string(),
            round_value(osu_diff.great_hit_window as f32, 2),
        );
    }
    snapshot.stats.pp.ss = crate::pp::calculator::calc_fc_pp(diff, mods_legacy);
    snapshot.stats.pp.fc = snapshot.stats.pp.ss;
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
        snapshot
            .stats
            .hit_window
            .values
            .insert("great".to_string(), 55.5);

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        assert!(json.contains("\"id\":12345"));
        assert!(json.contains("\"title\":\"Test Map\""));
        assert!(json.contains("\"great\":55.5"));
        assert!(!json.contains("\"pp\""));
    }
}
