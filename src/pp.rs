#[cfg(feature = "pp")]
pub mod calculator {
    use rosu_mods::GameModsLegacy;
    use rosu_pp::any::DifficultyAttributes;
    use rosu_pp::{Beatmap, Difficulty, Performance};
    use serde::Serialize;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Serialize, Default, PartialEq)]
    pub struct LivePpResult {
        pub current: f32,
        pub fc: f32,
    }

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
            .replace("V2", "")
            .replace("NF", "");
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

            let nm = parse_legacy_mods("NM");
            assert_eq!(nm.bits(), 0);

            let v2 = parse_legacy_mods("ScoreV2");
            assert_eq!(v2.bits(), 0);
        }

        #[test]
        fn test_gradual_chunk_progression() {
            // A minimal valid osu format v14 beatmap with 25 hitobjects
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

            // 25 objects stepped every 10 objects: chunks at 10, 20, 25 -> 3 chunks
            assert_eq!(chunks.len(), 3);

            // At 0 hits, PP is strictly 0.0 (CSR smooth start)
            let pp_0 = calc_live_pp_from_chunks(&chunks, mods, 0, 0, 0, 0, 0);
            assert_eq!(pp_0, 0.0);

            // At 10 hits (full combo 10), PP should be > 0.0
            let pp_10 = calc_live_pp_from_chunks(&chunks, mods, 10, 10, 0, 0, 0);
            assert!(pp_10 > 0.0, "PP at 10 objects should be positive");

            // At 25 hits (FC 25), PP should be higher than at 10
            let pp_25 = calc_live_pp_from_chunks(&chunks, mods, 25, 25, 0, 0, 0);
            assert!(pp_25 > pp_10, "PP at 25 objects should be greater than at 10");

            // FC PP should be calculated
            let fc_pp = calc_fc_pp(&chunks.last().unwrap(), mods);
            assert!(fc_pp > 0.0);
        }
    }
}

#[cfg(not(feature = "pp"))]
pub mod calculator {
    use serde::Serialize;

    #[derive(Debug, Clone, Serialize, Default, PartialEq)]
    pub struct LivePpResult {
        pub current: f32,
        pub fc: f32,
    }
}
