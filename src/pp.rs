use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PpBreakdown {
    pub aim: f32,
    pub speed: f32,
    pub accuracy: f32,
    pub difficulty: f32,
    pub flashlight: f32,
    pub total: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DetailedPp {
    pub current: PpBreakdown,
    pub fc: PpBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LivePpResult {
    pub current: f32,
    pub fc: f32,
    pub max_achieved: f32,
    pub max_achievable: f32,
    pub detailed: DetailedPp,
}

#[cfg(feature = "pp")]
pub mod calculator {
    use super::*;
    use rosu_mods::GameModsLegacy;
    use rosu_pp::any::DifficultyAttributes;
    use rosu_pp::{Beatmap, Difficulty, Performance};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory cache for gradual difficulty chunks (10-object stepping)
    /// Key: (map_id, mods_bits)
    static CHUNKS_CACHE: Mutex<Option<HashMap<(u32, u32), Arc<Vec<DifficultyAttributes>>>>> =
        Mutex::new(None);

    /// Precompute and cache gradual difficulty attributes every 10 objects
    pub fn get_or_compute_gradual_chunks(
        map_id: u32,
        rosu_map: &Beatmap,
        mods: GameModsLegacy,
    ) -> Arc<Vec<DifficultyAttributes>> {
        let key = (map_id, mods.bits());
        let mut lock = CHUNKS_CACHE.lock().unwrap();
        let cache = lock.get_or_insert_with(HashMap::new);

        if let Some(chunks) = cache.get(&key) {
            return chunks.clone();
        }

        let diff = Difficulty::new().mods(mods);
        let mut iter = rosu_pp::GradualDifficulty::new(diff, rosu_map);
        let mut chunks = Vec::new();
        let mut last_attrs = None;
        let mut obj_count = 0;

        while let Some(attrs) = iter.next() {
            obj_count += 1;
            if obj_count % 10 == 0 {
                chunks.push(attrs.clone());
            }
            last_attrs = Some(attrs);
        }

        if obj_count % 10 != 0 {
            if let Some(attrs) = last_attrs {
                chunks.push(attrs);
            }
        }

        if chunks.is_empty() {
            let full = Difficulty::new().mods(mods).calculate(rosu_map);
            chunks.push(full);
        }

        let arc_chunks = Arc::new(chunks);
        cache.insert(key, arc_chunks.clone());
        arc_chunks
    }

    /// Extract aim, speed, accuracy, flashlight, and total PP into PpBreakdown
    pub fn extract_pp_breakdown(attrs: &rosu_pp::any::PerformanceAttributes) -> PpBreakdown {
        match attrs {
            rosu_pp::any::PerformanceAttributes::Osu(osu) => PpBreakdown {
                aim: osu.pp_aim as f32,
                speed: osu.pp_speed as f32,
                accuracy: osu.pp_acc as f32,
                difficulty: 0.0,
                flashlight: osu.pp_flashlight as f32,
                total: osu.pp as f32,
            },
            rosu_pp::any::PerformanceAttributes::Taiko(taiko) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: taiko.pp_acc as f32,
                difficulty: taiko.pp_difficulty as f32,
                flashlight: 0.0,
                total: taiko.pp as f32,
            },
            rosu_pp::any::PerformanceAttributes::Catch(catch) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: 0.0,
                difficulty: 0.0,
                flashlight: 0.0,
                total: catch.pp as f32,
            },
            rosu_pp::any::PerformanceAttributes::Mania(mania) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: 0.0,
                difficulty: mania.pp_difficulty as f32,
                flashlight: 0.0,
                total: mania.pp as f32,
            },
        }
    }

    /// Calculate 100% SS Perfect Full Combo PP
    pub fn calc_fc_pp(diff_attrs: &DifficultyAttributes, mods: GameModsLegacy) -> f32 {
        let pp_attrs = Performance::new(diff_attrs.clone())
            .mods(mods)
            .accuracy(100.0)
            .misses(0)
            .calculate();
        pp_attrs.pp() as f32
    }

    /// Calculate live PP from precomputed gradual 10-object difficulty chunks
    pub fn calc_live_pp_from_chunks(
        chunks: &[DifficultyAttributes],
        mods: GameModsLegacy,
        combo: u32,
        n300: u32,
        n100: u32,
        n50: u32,
        n0: u32,
    ) -> f32 {
        let passed = n300 + n100 + n50 + n0;
        if passed == 0 || chunks.is_empty() {
            return 0.0;
        }

        let chunk_idx = ((passed.saturating_sub(1) / 10) as usize).min(chunks.len() - 1);
        let attrs = &chunks[chunk_idx];

        let pp = Performance::new(attrs.clone())
            .mods(mods)
            .combo(combo)
            .n300(n300)
            .n100(n100)
            .n50(n50)
            .misses(n0)
            .passed_objects(passed)
            .calculate()
            .pp();

        pp as f32
    }

    /// Calculate full live PP result including FC PP and detailed attribute breakdowns
    pub fn calc_detailed_live_and_fc_pp(
        chunks: &[DifficultyAttributes],
        mods: GameModsLegacy,
        combo: u32,
        n300: u32,
        n100: u32,
        n50: u32,
        n0: u32,
    ) -> LivePpResult {
        if chunks.is_empty() {
            return LivePpResult::default();
        }

        let last_attrs = chunks.last().unwrap();
        let fc_perf = Performance::new(last_attrs.clone())
            .mods(mods)
            .accuracy(100.0)
            .misses(0)
            .calculate();
        let fc_breakdown = extract_pp_breakdown(&fc_perf);
        let fc_total = fc_perf.pp() as f32;

        let passed = n300 + n100 + n50 + n0;
        if passed == 0 {
            return LivePpResult {
                current: 0.0,
                fc: fc_total,
                max_achieved: 0.0,
                max_achievable: fc_total,
                detailed: DetailedPp {
                    current: PpBreakdown::default(),
                    fc: fc_breakdown,
                },
            };
        }

        let chunk_idx = ((passed.saturating_sub(1) / 10) as usize).min(chunks.len() - 1);
        let live_attrs = &chunks[chunk_idx];

        let live_perf = Performance::new(live_attrs.clone())
            .mods(mods)
            .combo(combo)
            .n300(n300)
            .n100(n100)
            .n50(n50)
            .misses(n0)
            .passed_objects(passed)
            .calculate();
        let live_breakdown = extract_pp_breakdown(&live_perf);
        let live_total = live_perf.pp() as f32;

        LivePpResult {
            current: live_total,
            fc: fc_total,
            max_achieved: live_total,
            max_achievable: fc_total,
            detailed: DetailedPp {
                current: live_breakdown,
                fc: fc_breakdown,
            },
        }
    }

    /// Parse legacy bitmask into GameModsLegacy
    pub fn parse_mods_bits(mods_bits: u32) -> GameModsLegacy {
        GameModsLegacy::from_bits(mods_bits)
    }

    /// Parse legacy mod string (e.g. "HDHR", "DT") into GameModsLegacy
    pub fn parse_legacy_mods(mod_str: &str) -> GameModsLegacy {
        let clean = mod_str.trim().to_uppercase();
        if clean.is_empty() || clean == "NM" || clean == "NONE" {
            return GameModsLegacy::default();
        }
        let stripped = clean
            .replace("SCOREV2", "")
            .replace("SV2", "")
            .replace("V2", "");
        stripped.trim().parse().unwrap_or_default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_zero_passed_objects_returns_zero_pp() {
            let chunks = vec![];
            let pp = calc_live_pp_from_chunks(&chunks, GameModsLegacy::default(), 0, 0, 0, 0, 0);
            assert_eq!(pp, 0.0);
        }

        #[test]
        fn test_mod_parsing() {
            let hdhr = parse_legacy_mods("HDHR");
            assert!(hdhr.contains(GameModsLegacy::Hidden));
            assert!(hdhr.contains(GameModsLegacy::HardRock));
            assert!(!hdhr.contains(GameModsLegacy::DoubleTime));

            let nf = parse_legacy_mods("HDNF");
            assert!(nf.contains(GameModsLegacy::Hidden));
            assert!(nf.contains(GameModsLegacy::NoFail));

            let nm = parse_legacy_mods("NM");
            assert_eq!(nm.bits(), 0);

            let v2 = parse_legacy_mods("ScoreV2");
            assert_eq!(v2.bits(), 0);
        }

        #[test]
        fn test_gradual_chunk_progression() {
            let mut map_content = String::from(
                "osu file format v14\n\n[General]\nMode: 0\n\n[Metadata]\nTitle:Test\nArtist:Test\nCreator:Test\nVersion:Normal\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n"
            );
            for i in 0..25 {
                let time = 1000 + i * 200;
                map_content.push_str(&format!("256,192,{},1,0,0:0:0:0:\n", time));
            }

            let beatmap = Beatmap::from_bytes(map_content.as_bytes()).expect("parse map");
            let mods = GameModsLegacy::default();
            let chunks = get_or_compute_gradual_chunks(12345, &beatmap, mods);

            assert_eq!(chunks.len(), 3);

            let detailed_0 = calc_detailed_live_and_fc_pp(&chunks, mods, 0, 0, 0, 0, 0);
            assert_eq!(detailed_0.current, 0.0);
            assert!(detailed_0.fc > 0.0);
            assert!(detailed_0.detailed.fc.aim > 0.0 || detailed_0.detailed.fc.accuracy > 0.0);

            let detailed_25 = calc_detailed_live_and_fc_pp(&chunks, mods, 25, 25, 0, 0, 0);
            assert!(detailed_25.current > 0.0);
            assert_eq!(detailed_25.current, detailed_25.fc);
        }
    }
}

#[cfg(not(feature = "pp"))]
pub mod calculator {
    pub fn parse_mods_bits(_mods_bits: u32) -> u32 {
        _mods_bits
    }
}
