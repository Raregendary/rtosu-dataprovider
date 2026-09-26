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
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex, OnceLock};

    /// In-memory cache for gradual difficulty chunks.
    /// Key: (map_id_or_hash, mods_bits)
    static CHUNKS_CACHE: OnceLock<Mutex<HashMap<(u64, u32), Arc<Vec<DifficultyAttributes>>>>> =
        OnceLock::new();

    static IN_PROGRESS: OnceLock<Mutex<HashSet<(u64, u32)>>> = OnceLock::new();

    fn chunks_cache() -> &'static Mutex<HashMap<(u64, u32), Arc<Vec<DifficultyAttributes>>>> {
        CHUNKS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn in_progress() -> &'static Mutex<HashSet<(u64, u32)>> {
        IN_PROGRESS.get_or_init(|| Mutex::new(HashSet::new()))
    }

    fn insert_chunks(key: (u64, u32), chunks: Arc<Vec<DifficultyAttributes>>) {
        let mut cache = chunks_cache().lock().unwrap();
        if cache.len() >= 100 && !cache.contains_key(&key) {
            if let Some(old_key) = cache.keys().next().copied() {
                cache.remove(&old_key);
            }
        }
        cache.insert(key, chunks);
    }

    fn beatmap_cache_key(map_id: u32, map: &Beatmap) -> u64 {
        if map_id > 0 {
            map_id as u64
        } else {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            map.hit_objects.len().hash(&mut hasher);
            (map.mode as u8).hash(&mut hasher);
            if let Some(first) = map.hit_objects.first() {
                (first.start_time as i64).hash(&mut hasher);
            }
            if let Some(last) = map.hit_objects.last() {
                (last.start_time as i64).hash(&mut hasher);
            }
            hasher.finish() | 0x8000_0000_0000_0000
        }
    }

    /// Synchronously compute difficulty chunks for a given beatmap and mods.
    pub fn compute_chunks(
        rosu_map: &Beatmap,
        mods: GameModsLegacy,
        chunk_count: usize,
    ) -> Arc<Vec<DifficultyAttributes>> {
        let chunk_count = chunk_count.clamp(1, 250);
        let total_objects = rosu_map.hit_objects.len();

        if chunk_count <= 1 || total_objects == 0 {
            let full = Difficulty::new().mods(mods).calculate(rosu_map);
            return Arc::new(vec![full]);
        }

        let step = (total_objects / chunk_count).max(1);
        let diff = Difficulty::new().mods(mods);
        let mut iter = rosu_pp::GradualDifficulty::new(diff, rosu_map);
        let mut chunks = Vec::with_capacity(chunk_count + 1);
        let mut last_attrs = None;
        let mut obj_count = 0;

        while let Some(attrs) = iter.next() {
            obj_count += 1;
            if obj_count % step == 0 {
                chunks.push(attrs.clone());
            }
            last_attrs = Some(attrs);
        }

        if let Some(attrs) = last_attrs {
            if chunks.is_empty() || obj_count % step != 0 {
                chunks.push(attrs);
            }
        }

        if chunks.is_empty() {
            let full = Difficulty::new().mods(mods).calculate(rosu_map);
            chunks.push(full);
        }

        Arc::new(chunks)
    }

    /// Precompute and cache gradual difficulty attributes based on chunk_count (1..=250)
    pub fn get_or_compute_gradual_chunks(
        map_id: u32,
        rosu_map: &Beatmap,
        mods: GameModsLegacy,
        chunk_count: usize,
    ) -> Arc<Vec<DifficultyAttributes>> {
        let map_key = beatmap_cache_key(map_id, rosu_map);
        let key = (map_key, mods.bits());
        let chunk_count = chunk_count.clamp(1, 250);

        // 1. Check if already computed
        {
            let cache = chunks_cache().lock().unwrap();
            if let Some(chunks) = cache.get(&key) {
                if chunk_count <= 1 || chunks.len() > 1 {
                    crate::instr_scope!(PpChunksCached);
                    return chunks.clone();
                }
            }
        }

        // 2. If chunk_count == 1, calculate full map directly in ~5ms without gradual loop
        if chunk_count <= 1 {
            let full = Difficulty::new().mods(mods).calculate(rosu_map);
            let chunks = Arc::new(vec![full]);
            insert_chunks(key, chunks.clone());
            return chunks;
        }

        // 3. For small maps (< 1000 objects), computing is very fast (~20-50ms)
        let total_objects = rosu_map.hit_objects.len();
        if total_objects < 1000 {
            crate::instr_scope!(PpChunksCompute);
            let chunks = compute_chunks(rosu_map, mods, chunk_count);
            insert_chunks(key, chunks.clone());
            return chunks;
        }

        // 4. For larger maps (marathons, long songs):
        // Avoid blocking the poll loop! Spawn background task to compute full gradual chunks.
        let already_in_progress = {
            let mut in_prog = in_progress().lock().unwrap();
            !in_prog.insert(key)
        };

        if !already_in_progress {
            let map_clone = rosu_map.clone();
            std::thread::Builder::new()
                .name(format!("pp-chunk-{map_id}"))
                .spawn(move || {
                    let chunks = compute_chunks(&map_clone, mods, chunk_count);
                    insert_chunks(key, chunks);
                    in_progress().lock().unwrap().remove(&key);
                })
                .ok();
        }

        // Check if cache already has a fallback entry
        {
            let cache = chunks_cache().lock().unwrap();
            if let Some(chunks) = cache.get(&key) {
                return chunks.clone();
            }
        }

        // Temporary fallback while background thread is computing: instant full-map calculation
        let full = Difficulty::new().mods(mods).calculate(rosu_map);
        let fallback = Arc::new(vec![full]);
        insert_chunks(key, fallback.clone());
        fallback
    }

    /// Extract aim, speed, accuracy, flashlight, and total PP into PpBreakdown
    pub fn extract_pp_breakdown(attrs: &rosu_pp::any::PerformanceAttributes) -> PpBreakdown {
        match attrs {
            rosu_pp::any::PerformanceAttributes::Osu(osu) => PpBreakdown {
                aim: crate::beatmap::round_value(osu.pp_aim as f32, 2),
                speed: crate::beatmap::round_value(osu.pp_speed as f32, 2),
                accuracy: crate::beatmap::round_value(osu.pp_acc as f32, 2),
                difficulty: 0.0,
                flashlight: crate::beatmap::round_value(osu.pp_flashlight as f32, 2),
                total: crate::beatmap::round_value(osu.pp as f32, 2),
            },
            rosu_pp::any::PerformanceAttributes::Taiko(taiko) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: crate::beatmap::round_value(taiko.pp_acc as f32, 2),
                difficulty: crate::beatmap::round_value(taiko.pp_difficulty as f32, 2),
                flashlight: 0.0,
                total: crate::beatmap::round_value(taiko.pp as f32, 2),
            },
            rosu_pp::any::PerformanceAttributes::Catch(catch) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: 0.0,
                difficulty: 0.0,
                flashlight: 0.0,
                total: crate::beatmap::round_value(catch.pp as f32, 2),
            },
            rosu_pp::any::PerformanceAttributes::Mania(mania) => PpBreakdown {
                aim: 0.0,
                speed: 0.0,
                accuracy: 0.0,
                difficulty: crate::beatmap::round_value(mania.pp_difficulty as f32, 2),
                flashlight: 0.0,
                total: crate::beatmap::round_value(mania.pp as f32, 2),
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

    pub fn calc_accuracy_pp(map: &rosu_pp::Beatmap, mods: u32, accuracy: f64) -> f32 {
        let mods = parse_mods_bits(mods);
        let difficulty = rosu_pp::Difficulty::new().mods(mods).calculate(map);
        Performance::new(difficulty)
            .accuracy(accuracy)
            .calculate()
            .pp() as f32
    }

    pub fn calc_accuracy_table_from_diff(
        diff: &DifficultyAttributes,
    ) -> crate::v2::PerformanceAccuracy {
        let calc = |acc: f64| {
            let pp = Performance::new(diff.clone())
                .accuracy(acc)
                .hitresult_generator::<rosu_pp::any::hitresult_generator::Fast>()
                .lazer(false)
                .calculate()
                .pp();
            crate::beatmap::round_value(pp as f32, 2)
        };
        crate::v2::PerformanceAccuracy {
            n90: calc(90.0),
            n91: calc(91.0),
            n92: calc(92.0),
            n93: calc(93.0),
            n94: calc(94.0),
            n95: calc(95.0),
            n96: calc(96.0),
            n97: calc(97.0),
            n98: calc(98.0),
            n99: calc(99.0),
            n100: calc(100.0),
        }
    }

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
        total_objects: usize,
        mods: GameModsLegacy,
        combo: u32,
        n300: u32,
        n100: u32,
        n50: u32,
        n0: u32,
    ) -> LivePpResult {
        crate::instr_scope!(PpLive);
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
        let fc_total = crate::beatmap::round_value(fc_perf.pp() as f32, 2);

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

        let chunk_idx = if chunks.len() <= 1 || total_objects == 0 {
            0
        } else {
            let passed_usize = passed as usize;
            ((passed_usize * (chunks.len() - 1)) / total_objects).min(chunks.len() - 1)
        };
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
        let live_total = crate::beatmap::round_value(live_perf.pp() as f32, 2);

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

    /// Extract live star rating based on passed object count
    pub fn live_stars_from_chunks(
        chunks: &[DifficultyAttributes],
        total_objects: usize,
        passed_objects: u32,
    ) -> f32 {
        if chunks.is_empty() || total_objects == 0 || passed_objects == 0 {
            return 0.0;
        }
        let passed_usize = passed_objects as usize;
        let chunk_idx = ((passed_usize * (chunks.len() - 1)) / total_objects).min(chunks.len() - 1);
        crate::beatmap::round_value(chunks[chunk_idx].stars() as f32, 2)
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
                "osu file format v14\n\n[General]\nMode: 0\n\n[Metadata]\nTitle:Test\nArtist:Test\nCreator:Test\nVersion:Normal\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n",
            );
            for i in 0..25 {
                let time = 1000 + i * 200;
                map_content.push_str(&format!("256,192,{},1,0,0:0:0:0:\n", time));
            }

            let beatmap = Beatmap::from_bytes(map_content.as_bytes()).expect("parse map");
            let mods = GameModsLegacy::default();

            // 1. Test chunk_count = 1 (single chunk / no gradual)
            let chunks_single = get_or_compute_gradual_chunks(99999, &beatmap, mods, 1);
            assert_eq!(chunks_single.len(), 1);

            // 2. Test gradual chunk calculation
            let chunks = get_or_compute_gradual_chunks(12345, &beatmap, mods, 5);
            assert!(chunks.len() >= 4);

            let detailed_0 = calc_detailed_live_and_fc_pp(&chunks, 25, mods, 0, 0, 0, 0, 0);
            assert_eq!(detailed_0.current, 0.0);
            assert!(detailed_0.fc > 0.0);
            assert!(detailed_0.detailed.fc.aim > 0.0 || detailed_0.detailed.fc.accuracy > 0.0);

            let detailed_25 = calc_detailed_live_and_fc_pp(&chunks, 25, mods, 25, 25, 0, 0, 0);
            assert!(detailed_25.current > 0.0);
            assert_eq!(detailed_25.current, detailed_25.fc);
        }

        #[test]
        fn test_unsubmitted_map_cache_key_differentiation() {
            let map1_content = "osu file format v14\n\n[General]\nMode: 0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n";
            let map2_content = "osu file format v14\n\n[General]\nMode: 0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n256,192,2000,1,0,0:0:0:0:\n";

            let b1 = Beatmap::from_bytes(map1_content.as_bytes()).expect("parse map1");
            let b2 = Beatmap::from_bytes(map2_content.as_bytes()).expect("parse map2");

            let key1 = beatmap_cache_key(0, &b1);
            let key2 = beatmap_cache_key(0, &b2);

            // Both have top bit set
            assert_ne!(key1 & 0x8000_0000_0000_0000, 0);
            assert_ne!(key2 & 0x8000_0000_0000_0000, 0);

            // Different unsubmitted maps produce distinct keys
            assert_ne!(key1, key2);

            // Submitted maps use their ID directly
            assert_eq!(beatmap_cache_key(12345, &b1), 12345);
        }
    }
}

#[cfg(not(feature = "pp"))]
pub mod calculator {
    pub fn parse_mods_bits(_mods_bits: u32) -> u32 {
        _mods_bits
    }
}
