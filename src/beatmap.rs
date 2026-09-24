use crate::address::checked_add_signed;
use crate::process::ProcessMemory;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    pub slider_factor: f32,
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
    pub max_combo: i32,
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
    pub folder: String,
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
        2 => "catch",
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
    let beatmap_ptr = memory
        .read_pointer(beatmap_ptr_addr)
        .context("reading beatmap pointer table")?;
    let beatmap_addr = memory
        .read_pointer(beatmap_ptr)
        .context("dereferencing beatmap pointer")?;

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

    let id = memory.read_i32(checked_add_signed(beatmap_addr, 0x78)?).unwrap_or(0);
    let set_id = memory.read_i32(checked_add_signed(beatmap_addr, 0x7C)?).unwrap_or(0);
    let status_num = memory.read_i32(checked_add_signed(beatmap_addr, 0x80)?).unwrap_or(0);

    let ar = memory.read_f32(checked_add_signed(beatmap_addr, 0x90)?).unwrap_or(0.0);
    let cs = memory.read_f32(checked_add_signed(beatmap_addr, 0x94)?).unwrap_or(0.0);
    let hp = memory.read_f32(checked_add_signed(beatmap_addr, 0x98)?).unwrap_or(0.0);
    let od = memory.read_f32(checked_add_signed(beatmap_addr, 0x9C)?).unwrap_or(0.0);

    let title = read_net_string(memory, beatmap_addr, 0x64, pointer_width).unwrap_or_default();
    let artist = read_net_string(memory, beatmap_addr, 0x68, pointer_width).unwrap_or_default();
    let mapper = read_net_string(memory, beatmap_addr, 0x6C, pointer_width).unwrap_or_default();
    let version = read_net_string(memory, beatmap_addr, 0x70, pointer_width).unwrap_or_default();
    let folder = read_net_string(memory, beatmap_addr, 0x74, pointer_width).unwrap_or_default();
    let checksum = read_net_string(memory, beatmap_addr, 0xB4, pointer_width).unwrap_or_default();

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
            number: 0,
            name: "osu".to_string(),
        },
        artist: artist.clone(),
        artist_unicode: artist,
        title: title.clone(),
        title_unicode: title,
        mapper,
        version,
        folder,
        source: String::new(),
        tags: String::new(),
        stats: BeatmapStats {
            stars: StarsBreakdown::default(),
            ar: StatValue { original: ar, converted: ar },
            cs: StatValue { original: cs, converted: cs },
            od: StatValue { original: od, converted: od },
            hp: StatValue { original: hp, converted: hp },
            bpm: BpmStats::default(),
            objects: ObjectCounts::default(),
            max_combo: 0,
            pp: BeatmapPpStats::default(),
        },
    })
}

/// Read a .NET UTF-16 String from memory at offset from base
fn read_net_string(
    memory: &ProcessMemory,
    base: u64,
    offset: i64,
    _pointer_width: usize,
) -> Result<String> {
    let str_ptr_addr = checked_add_signed(base, offset)?;
    let str_ptr = memory.read_pointer(str_ptr_addr)?;
    if str_ptr == 0 {
        return Ok(String::new());
    }

    let len = memory.read_i32(checked_add_signed(str_ptr, 0x4)?)?;
    if len <= 0 || len > 1024 {
        return Ok(String::new());
    }

    let utf16_addr = checked_add_signed(str_ptr, 0x8)?;
    let mut u16_chars = Vec::with_capacity(len as usize);
    for i in 0..len {
        let char_addr = checked_add_signed(utf16_addr, (i as i64) * 2)?;
        let c = memory.read_u16(char_addr)?;
        u16_chars.push(c);
    }

    Ok(String::from_utf16_lossy(&u16_chars))
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

        let json = serde_json::to_string(&snapshot).expect("serialize beatmap");
        assert!(json.contains("\"id\":12345"));
        assert!(json.contains("\"title\":\"Test Map\""));
        assert!(json.contains("\"ss\":512.4"));
    }
}
