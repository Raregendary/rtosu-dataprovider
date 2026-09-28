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
    use std::sync::{Arc, Mutex, OnceLock};

    /// Whole-map difficulty attributes, keyed by `(map, mods)`.
    ///
    /// One `Difficulty::calculate` pass, shared. The solo session keeps its own
    /// copy in `cached_difficulty_attrs` because it has one already for the
    /// accuracy table; this is for the paths that do not, above all the
    /// tournament spectator loop, which holds a `&mut` borrow of the session for
    /// the whole pass and so cannot reach a session-owned cache from inside it.
    ///
    /// A tournament puts every client on the same map and mods, so a process-wide
    /// cache means the pass runs once rather than once per client. The value is a
    /// single attributes set, not a curve, so there is nothing big to keep.
    type FullDiffCache = HashMap<(u64, u32), Arc<DifficultyAttributes>>;
    static FULL_DIFF: OnceLock<Mutex<FullDiffCache>> = OnceLock::new();

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

    /// A forward-only window onto a map's difficulty curve.
    ///
    /// `rosu_pp::GradualDifficulty` is an iterator: each `next()` folds exactly
    /// one more object into the strain state and hands back the attributes for
    /// the map considered *so far*. Holding it and stepping it as objects are
    /// actually judged is the intended use.
    ///
    /// This replaces a precomputed chunk vector, which precomputed the whole
    /// curve at map load so it could index it by hit count. That inverted the
    /// cost in two ways. An untouched map cost as much as a finished one, and on
    /// a big map it had to go to a worker thread because it blew the poll
    /// budget -- and `stars.live` then reported exactly `stars.total` on every
    /// map of 1000 objects or more, because the worker had not finished and the
    /// placeholder was indistinguishable from a real answer. See
    /// `the_cursor_reports_a_partial_rating_on_a_map_over_the_old_chunk_ceiling`.
    ///
    /// What the step costs is worth being precise about, because it is not the
    /// constant people assume. `DifficultyValues::eval` re-reads the strain
    /// peaks accumulated so far, so a step deep into a map costs more than one
    /// near the start. Measured per judgement, release, on circles a tenth of a
    /// second apart:
    ///
    /// | map size | per judgement | whole walk |
    /// | -------- | ------------- | ---------- |
    /// |  1 000   |    73 us      |   60 ms    |
    /// |  5 000   |   251 us      |  919 ms    |
    /// | 15 000   |   689 us      | 7.2 s      |
    /// | 30 000   |  1 651 us     | 32.1 s     |
    ///
    /// So a full play costs about what the old precompute cost in total, but
    /// spread across the objects actually judged instead of paid up front, and
    /// the value is exact rather than quantised. On the map this was written
    /// for -- 1035 objects, 176 judged -- that is ~5 ms of CPU for the whole run
    /// against a 56 ms blocking stall at map load, and a correct number.
    ///
    /// tosu is on the same footing: `beatmapPP.currAttributes` is likewise the
    /// gradual rating at the current position, recomputed as objects are passed.
    /// Matching it means accepting this cost.
    ///
    /// The curve only moves forward, so a retried attempt (the judged count
    /// going *backwards*) cannot be served from the state built so far.
    /// `advance_to` rebuilds from the caller's map in that case rather than
    /// reporting a stale rating, which is why it takes the map rather than
    /// owning one.
    pub struct GradualCursor {
        /// `(map, mods)` this curve was built for. A change means a rebuild.
        key: (u64, u32),
        /// Objects folded into `iter` so far. The curve's current position.
        processed: u32,
        /// The attributes at `processed`. `next()` returns them by value, and
        /// the caller wants a borrow it can hold across a later `advance_to`
        /// on the same object, so the last one is kept.
        current: Option<DifficultyAttributes>,
        /// `None` until the first `advance_to`, and after a rebuild starts.
        iter: Option<rosu_pp::GradualDifficulty>,
    }

    impl GradualCursor {
        /// A cursor that has folded nothing. The first `advance_to` builds it.
        pub fn new() -> Self {
            Self {
                key: (0, 0),
                processed: 0,
                current: None,
                iter: None,
            }
        }

        /// Drop the curve. The next `advance_to` rebuilds it from scratch.
        pub fn clear(&mut self) {
            *self = Self::new();
        }

        /// How many objects have been folded in.
        pub fn processed(&self) -> u32 {
            self.processed
        }

        fn build(map: &Beatmap, mods: GameModsLegacy) -> Option<rosu_pp::GradualDifficulty> {
            let diff = Difficulty::new().mods(mods);
            rosu_pp::GradualDifficulty::new_with_mode(diff, map, map.mode).ok()
        }

        /// The attributes for the first `passed` objects, stepping the curve
        /// forward to reach them.
        ///
        /// `passed` is clamped to the map's object count. Returns `None` for an
        /// empty map or one the ruleset cannot convert, which is the same
        /// "nothing to report" answer the chunk path gave.
        pub fn advance_to(
            &mut self,
            map_id: u32,
            map: &Beatmap,
            mods: GameModsLegacy,
            passed: u32,
        ) -> Option<&DifficultyAttributes> {
            let key = (beatmap_cache_key(map_id, map), mods.bits());
            let total = map.hit_objects.len() as u32;
            let target = passed.min(total);

            // A new map, new mods, or a retried attempt all invalidate the
            // curve: the first two change what is being folded, and the third
            // asks for a position the curve has already passed.
            if self.iter.is_none() || self.key != key || self.processed > target {
                let Some(iter) = Self::build(map, mods) else {
                    self.clear();
                    return None;
                };
                self.key = key;
                self.processed = 0;
                self.current = None;
                self.iter = Some(iter);
            }

            while self.processed < target {
                let next = self.iter.as_mut().and_then(Iterator::next);
                match next {
                    Some(attrs) => self.current = Some(attrs),
                    // The curve ran out before the judged count did, which means
                    // the count came from a longer map. Nothing left to report.
                    None => break,
                }
                self.processed += 1;
            }

            self.current.as_ref()
        }
    }

    /// The difficulty of the whole map under `mods`.
    ///
    /// This is what the FC pp and `stats.total` are properties of. It is a
    /// single O(objects) pass, done once per `(map, mods)`, and it is
    /// deliberately *not* how `stars.live` is computed -- that is the gradual
    /// rating at the objects judged so far, which is what `GradualCursor` holds.
    pub fn full_difficulty(
        map_id: u32,
        map: &Beatmap,
        mods: GameModsLegacy,
    ) -> Arc<DifficultyAttributes> {
        let key = (beatmap_cache_key(map_id, map), mods.bits());
        let cache = FULL_DIFF.get_or_init(|| Mutex::new(HashMap::new()));
        {
            let guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = guard.get(&key) {
                return Arc::clone(cached);
            }
        }
        let computed = Arc::new(Difficulty::new().mods(mods).calculate(map));
        let mut guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.len() >= 16
            && !guard.contains_key(&key)
            && let Some(oldest) = guard.keys().next().copied()
        {
            guard.remove(&oldest);
        }
        guard.insert(key, Arc::clone(&computed));
        computed
    }

    impl Default for GradualCursor {
        fn default() -> Self {
            Self::new()
        }
    }

    // SAFETY: `rosu_pp::GradualDifficulty` holds `Rc<RefCell<..>>` in its taiko
    // and mania variants, so it is neither `Send` nor `Sync`, and a struct
    // wrapping it cannot be `Send` without this assertion. `OsuReader` is
    // required to be `Send` (`reader::tests::the_reader_can_be_moved_to_another_thread`)
    // and it owns both sessions, so without it the whole change is unbuildable.
    //
    // The assertion rests on the `Rc`s never being *shared*:
    //
    // * every cursor method takes `&mut self`, and a session is polled through
    //   `&mut`, so at most one thread holds a given curve at a time;
    // * the one place a cursor legitimately crosses a thread boundary is the
    //   init scope in `TournamentSession::poll`, whose workers *construct* client
    //   states and hand them to the poll thread. A freshly constructed cursor has
    //   no curve yet, and moving a value that was never shared between threads is
    //   sound -- it is cloning a live `Rc` across threads that is not.
    //
    // If a session is ever driven concurrently from two threads, this stops
    // being true and this impl has to be replaced with real synchronisation.
    unsafe impl Send for GradualCursor {}

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

    /// Calculate full live PP result including FC PP and detailed attribute breakdowns.
    ///
    /// `live_attrs` is the gradual rating at the objects judged so far, from
    /// `GradualCursor`; `full_attrs` is the whole-map difficulty the session
    /// already caches for `beatmap.stats.stars.total` and the accuracy table.
    /// Splitting them is what makes the FC honest: it is a property of the
    /// entire map, so it must not be read off whatever partial rating happened
    /// to be current.
    ///
    /// `live_attrs` is `None` when nothing is judged yet, which is every tick
    /// outside a play: `current` is then 0 and the FC is still real, matching
    /// tosu's zeroed-current-against-a-real-fc shape in song select.
    pub fn calc_detailed_live_and_fc_pp(
        live_attrs: Option<&DifficultyAttributes>,
        full_attrs: &DifficultyAttributes,
        mods: GameModsLegacy,
        combo: u32,
        n300: u32,
        n100: u32,
        n50: u32,
        n0: u32,
    ) -> LivePpResult {
        crate::instr_scope!(PpLive);
        let fc_perf = Performance::new(full_attrs.clone())
            .mods(mods)
            .accuracy(100.0)
            .misses(0)
            .calculate();
        let fc_breakdown = extract_pp_breakdown(&fc_perf);
        let fc_total = crate::beatmap::round_value(fc_perf.pp() as f32, 2);

        let passed = n300 + n100 + n50 + n0;
        let Some(live_attrs) = live_attrs.filter(|_| passed > 0) else {
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
        };

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

    /// tosu's `beatmap.stats.stars.live` (`buildResultV2.ts:819`) is
    /// `beatmapPP.currAttributes.stars` -- the *gradual* rating of the objects
    /// passed so far, not the whole map. That is precisely the cursor's current
    /// attributes, so there is nothing left to index: the value is whatever the
    /// curve says at `passed`, to two decimals like every other star value.
    pub fn live_stars(attrs: &DifficultyAttributes) -> f32 {
        crate::beatmap::round_value(attrs.stars() as f32, 2)
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

        fn circle_map(objects: usize) -> Beatmap {
            let mut map_content = String::from(
                "osu file format v14\n\n[General]\nMode: 0\n\n[Metadata]\nTitle:Test\nArtist:Test\nCreator:Test\nVersion:Normal\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n",
            );
            for i in 0..objects as u32 {
                map_content.push_str(&format!("256,192,{},1,0,0:0:0:0:\n", 1000 + i * 100));
            }
            Beatmap::from_bytes(map_content.as_bytes()).expect("parse map")
        }

        #[test]
        fn the_cursor_reports_the_rating_of_the_objects_actually_judged() {
            let beatmap = circle_map(200);
            let mods = GameModsLegacy::default();
            let total_objects = beatmap.hit_objects.len();
            assert_eq!(total_objects, 200);

            let full = Difficulty::new().calculate(&beatmap).stars();
            assert!(full > 0.0);

            let mut cursor = GradualCursor::new();
            let judged = 16u32;
            let at_start = cursor
                .advance_to(7, &beatmap, mods, judged)
                .map(live_stars)
                .expect("attributes at 16 objects");
            assert_eq!(cursor.processed(), judged);

            let mut cursor2 = GradualCursor::new();
            let at_half = cursor2
                .advance_to(7, &beatmap, mods, 100)
                .map(live_stars)
                .expect("attributes at 100 objects");
            let at_end = cursor2
                .advance_to(7, &beatmap, mods, total_objects as u32)
                .map(live_stars)
                .expect("attributes at the last object");

            // The point of this test: 16 of 200 objects is nowhere near the whole
            // map, so the value must be the difficulty of *those* objects, not of
            // a whole bucket and above all not of the map.
            assert!(
                at_start < full as f32,
                "16/200 objects reported {at_start}, which is the full rating {full}"
            );
            assert!(at_start < at_half, "{at_start} should be below {at_half}");
            assert!(at_half < at_end, "{at_half} should be below {at_end}");
            assert!(
                (at_end - full as f32).abs() < 0.01,
                "a fully judged map should report the full rating: {at_end} vs {full}"
            );
        }

        /// The regression this whole design exists to close.
        ///
        /// `get_or_compute_gradual_chunks` computed the curve synchronously
        /// below 1000 objects and handed a one-element placeholder above it,
        /// which `live_stars_from_chunks` could not tell from a real
        /// single-chunk answer -- so `stars.live` collapsed to `stars.total` on
        /// every map of 1000 objects or more, and the value latched because the
        /// poll loop only recomputed when a judgement changed.
        ///
        /// Measured live on map 4390203 (1035 objects, 176 judged) with tosu on
        /// :24050: rtosu reported `stars.live` 6.05 against tosu's 5.37, and
        /// 6.05 was `stars.total` exactly. The same failure is on record for
        /// map 2964306 at 6.06 vs 2.42.
        ///
        /// There is no size threshold left to cross, so this pins the value at
        /// both sides of where it used to be.
        #[test]
        fn the_cursor_reports_a_partial_rating_on_a_map_over_the_old_chunk_ceiling() {
            let mods = GameModsLegacy::default();

            for objects in [999usize, 1035] {
                let beatmap = circle_map(objects);
                let full = Difficulty::new().calculate(&beatmap).stars();
                assert!(full > 0.0);

                let mut cursor = GradualCursor::new();
                let judged = (objects as u32) / 6;
                let partial = cursor
                    .advance_to(4242, &beatmap, mods, judged)
                    .map(live_stars)
                    .unwrap_or(0.0);

                assert!(
                    partial < full as f32,
                    "{objects} objects, {judged} judged: reported {partial}, which is \
                     the full rating {full} -- the value collapsed"
                );
                assert!(
                    partial > 0.0,
                    "{objects} objects, {judged} judged: reported {partial}, which \
                     claims the played objects have no difficulty at all"
                );
            }
        }

        /// The cursor carries its state, so a play pays for the objects it
        /// actually judges and not for the whole map up front -- which is the
        /// property the chunk design could not have: it had to precompute the
        /// entire curve to be able to index it.
        ///
        /// Pinned behaviourally, because a timing assertion would be flaky:
        /// stepping one object at a time has to land on exactly the same curve
        /// as jumping straight there, which is only true if the state really is
        /// carried rather than recomputed.
        #[test]
        fn the_cursor_folds_each_object_once_and_carries_its_state_forward() {
            let beatmap = circle_map(60);
            let mods = GameModsLegacy::default();
            let total = beatmap.hit_objects.len() as u32;

            let mut stepwise = GradualCursor::new();
            let mut direct = GradualCursor::new();
            for object in 1..=total {
                let stepped = stepwise
                    .advance_to(11, &beatmap, mods, object)
                    .map(live_stars)
                    .expect("stepped");
                assert_eq!(stepwise.processed(), object);
                let jumped = direct
                    .advance_to(11, &beatmap, mods, object)
                    .map(live_stars)
                    .expect("direct");
                assert_eq!(
                    stepped, jumped,
                    "stepping to {object} one at a time must equal jumping there"
                );
            }

            // Asking for the same position again must not advance the curve, or
            // a paused game would keep folding objects that were never judged.
            let before = stepwise.processed();
            let _ = stepwise.advance_to(11, &beatmap, mods, total);
            assert_eq!(stepwise.processed(), before);
        }

        /// A retried attempt sends the judged count *backwards*, and a
        /// forward-only curve cannot answer that from the state it has built.
        /// It has to restart rather than report a rating for a position the play
        /// has already left.
        #[test]
        fn a_retry_restarts_the_curve_instead_of_reporting_a_stale_rating() {
            let beatmap = circle_map(80);
            let mods = GameModsLegacy::default();
            let full = Difficulty::new().calculate(&beatmap).stars();

            let mut cursor = GradualCursor::new();
            let deep = cursor
                .advance_to(5, &beatmap, mods, 70)
                .map(live_stars)
                .expect("deep into the map");
            assert_eq!(cursor.processed(), 70);
            assert!(deep > 0.0);

            // Retry: the count goes back to near zero.
            let restarted = cursor
                .advance_to(5, &beatmap, mods, 3)
                .map(live_stars)
                .expect("after the retry");
            assert_eq!(cursor.processed(), 3);

            // A fresh cursor at the same position must agree, or the restart
            // silently served a rating from the abandoned attempt.
            let mut fresh = GradualCursor::new();
            let expected = fresh
                .advance_to(5, &beatmap, mods, 3)
                .map(live_stars)
                .expect("fresh");
            assert_eq!(restarted, expected);
            assert_ne!(deep, restarted, "the abandoned attempt's rating survived");
            assert!((full as f32 - restarted) > 0.0);
        }

        /// Changing mods has to invalidate the curve: the attributes it holds
        /// are for the mods it was built with.
        #[test]
        fn changing_mods_rebuilds_the_curve() {
            let beatmap = circle_map(120);
            let nm = GameModsLegacy::default();
            let dt = GameModsLegacy::DoubleTime;

            let mut cursor = GradualCursor::new();
            let at_nm = cursor
                .advance_to(77, &beatmap, nm, 60)
                .map(live_stars)
                .expect("NM");
            let at_dt = cursor
                .advance_to(77, &beatmap, dt, 60)
                .map(live_stars)
                .expect("DT");
            assert_ne!(at_nm, at_dt, "DoubleTime must not report the NoMod rating");

            // And back again, at the same position on the same map.
            let back_to_nm = cursor
                .advance_to(77, &beatmap, nm, 60)
                .map(live_stars)
                .expect("NM again");
            assert_eq!(back_to_nm, at_nm);
        }

        /// `passed == 0` has to read as 0 stars, not as the first object's
        /// difficulty. Without the guard, a play state with nothing judged yet
        /// reports a nonzero rating, and song select reports one too.
        #[test]
        fn no_objects_judged_reads_as_zero_stars() {
            let beatmap = circle_map(50);
            let mods = GameModsLegacy::default();
            let mut cursor = GradualCursor::new();

            assert_eq!(
                cursor.advance_to(1, &beatmap, mods, 0).map(live_stars),
                None
            );
            assert_eq!(cursor.processed(), 0);

            // A map with no objects at all has no curve to walk.
            let empty = circle_map(0);
            let mut cursor2 = GradualCursor::new();
            assert_eq!(
                cursor2.advance_to(1, &empty, mods, 10).map(live_stars),
                None
            );

            // And a judged count past the end clamps to the last object rather
            // than running off it.
            let mut cursor3 = GradualCursor::new();
            let clamped = cursor3
                .advance_to(1, &beatmap, mods, 9_999)
                .map(live_stars)
                .expect("clamped");
            assert_eq!(cursor3.processed(), 50);
            let full = Difficulty::new().calculate(&beatmap).stars();
            assert!((clamped - full as f32).abs() < 0.01);
        }

        #[test]
        fn the_fc_is_a_property_of_the_whole_map_not_of_the_partial_rating() {
            let beatmap = circle_map(120);
            let mods = GameModsLegacy::default();
            let total_objects = beatmap.hit_objects.len();
            let full_attrs = Difficulty::new().mods(mods).calculate(&beatmap);

            let mut cursor = GradualCursor::new();
            let live_attrs = cursor
                .advance_to(3, &beatmap, mods, 20)
                .expect("partial attributes");

            // Nothing judged: current is zero, fc is real.
            let nothing = calc_detailed_live_and_fc_pp(None, &full_attrs, mods, 0, 0, 0, 0, 0);
            assert_eq!(nothing.current, 0.0);
            assert!(nothing.fc > 0.0);

            // Part way: both real, and the FC is the same number either way --
            // it is not read off the partial rating.
            let partial =
                calc_detailed_live_and_fc_pp(Some(live_attrs), &full_attrs, mods, 20, 20, 0, 0, 0);
            assert!(partial.current > 0.0);
            assert_eq!(partial.fc, nothing.fc);
            assert_eq!(partial.max_achievable, nothing.fc);

            // Fully judged: current has caught the FC.
            let mut cursor2 = GradualCursor::new();
            let end_attrs = cursor2
                .advance_to(3, &beatmap, mods, total_objects as u32)
                .expect("final attributes");
            let complete = calc_detailed_live_and_fc_pp(
                Some(end_attrs),
                &full_attrs,
                mods,
                total_objects as u32,
                total_objects as u32,
                0,
                0,
                0,
            );
            assert_eq!(complete.current, complete.fc);
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
