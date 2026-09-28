use crate::address::checked_add_signed;
use crate::beatmap::BeatmapSnapshot;
use crate::client::{
    GameplayState, LocalProfile, TournamentUser, find_pattern, is_tournament_manager_cmd,
    parse_spectate_client_arg, read_local_profile, read_tournament_user,
};
use crate::pattern::BytePattern;
use crate::process::{ProcessMemory, list_processes};
use crate::profile::{ClientProfile, load_profile};
use crate::tournament::{TournamentState, read_tournament_chat, read_tournament_state};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "pp")]
use crate::client::mod_bits;

/// How often the in-game score list is re-walked during a play.
///
/// The list only changes when a score is actually submitted or the player's own
/// row moves, so reading it on the poll tick spends kernel transitions on data
/// that has not changed. A second is well inside what an overlay can show.
const LEADERBOARD_INTERVAL: Duration = Duration::from_secs(1);

/// The inputs a tournament client's live PP was computed from: the raw mod bits,
/// the five judgement numbers, and the hit total the difficulty curve is
/// advanced by.
///
/// Named because it appears in [`CachedClientState::cached_live_pp`], where
/// spelling the tuple out is unreadable.
#[cfg(feature = "pp")]
type LivePpKey = (u32, u32, u32, u32, u32, u32, u32);

#[derive(Debug, Clone, Serialize)]
pub struct TournamentClientView {
    pub pid: u32,
    pub ipc_id: usize,
    pub team: String,
    pub user: Option<TournamentUser>,
    pub gameplay: Option<GameplayState>,
    pub beatmap: Option<BeatmapSnapshot>,
    pub pp: Option<crate::pp::LivePpResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TournamentSnapshot {
    pub captured_at_ms: u128,
    pub poll_duration_us: u128,
    pub manager: Option<TournamentState>,
    pub profile: Option<LocalProfile>,
    pub beatmap: Option<BeatmapSnapshot>,
    pub game_folder: String,
    pub songs_folder: String,
    pub skin_folder: String,
    pub game_time: i32,
    pub clients: Vec<TournamentClientView>,
    pub performance: crate::v2::PerformanceState,
    pub focused: bool,
}

pub struct CachedClientState {
    pub pid: u32,
    pub memory: ProcessMemory,
    pub command_line: String,
    pub ipc_id: Option<usize>,
    pub is_manager: bool,
    pub is_spectator: bool,
    pub ruleset_container_addr: Option<u64>,
    pub spectating_user_pattern_addr: Option<u64>,
    pub chat_engine_pattern_addr: Option<u64>,
    pub base_pattern_addr: Option<u64>,
    pub play_time_pattern_addr: Option<u64>,
    pub audio_length_pattern_addr: Option<u64>,
    pub game_time_pattern_addr: Option<u64>,
    pub skin_pattern_addr: Option<u64>,
    pub user_profile_pattern_addr: Option<u64>,
    pub raw_login_status_pattern_addr: Option<u64>,
    pub game_folder: String,
    pub songs_folder: String,
    pub current_checksum: String,
    #[cfg(feature = "pp")]
    pub cached_beatmap: Option<rosu_pp::Beatmap>,
    pub cached_total_hits: u32,
    /// The live PP last computed for this client, and the judgement counts it
    /// was computed from.
    ///
    /// **Why this exists:** the rating is a pure function of (map, mods, combo,
    /// hit counts), and the hit counts only move when a note is judged. Without
    /// this, every client paid two `rosu_pp::Performance` evaluations on every
    /// tick whether or not anything had been hit -- at 60 Hz with 16 clients
    /// that is the per-tick cost of a full PP recalculation times 16, for a
    /// value that was usually identical to the one already published.
    ///
    /// Per client, not per session: each client is at a different point in the
    /// same map, so they legitimately differ. The `gradual_cursor` already made
    /// the *curve* incremental; this makes the *evaluation* conditional.
    ///
    /// The stored counts are the raw mod bits plus the five judgement numbers
    /// and their total, so a mod change, a retry or a new judgement all
    /// recompute rather than reusing a rating from a different situation.
    #[cfg(feature = "pp")]
    pub cached_live_pp: Option<(LivePpKey, crate::pp::LivePpResult)>,
    pub cached_hit_errors: Arc<[i16]>,
    pub cached_unstable_rate: f64,
    pub cached_gameplay: Option<GameplayState>,
    /// The live difficulty curve for this client's play. Per client, because
    /// each one is at a different point in the same map and the curve only
    /// moves forward: sharing one would make a client that is behind rebuild it
    /// on every tick.
    #[cfg(feature = "pp")]
    pub gradual_cursor: crate::pp::calculator::GradualCursor,
    pub cached_beatmap_ptr: u64,
    /// Difficulty id of the map behind `cached_beatmap_snapshot`, so a map swap
    /// that reuses the same beatmap object is still noticed.
    pub cached_beatmap_id: i32,
    pub cached_beatmap_snapshot: Option<BeatmapSnapshot>,
    pub cached_user_ptr: u64,
    pub cached_user: Option<TournamentUser>,
    pub cached_chat_key: Option<crate::tournament::ChatCacheKey>,
    pub cached_chat: Vec<crate::tournament::TournamentChatMessage>,
    pub last_pattern_retry: Instant,
}

pub struct TournamentSession {
    profile: ClientProfile,
    pointer_width: Option<usize>,
    scan_limit_bytes: usize,
    clients: BTreeMap<u32, CachedClientState>,
    last_proc_scan: Instant,
    pub enable_chat: bool,
    pub enable_pp: bool,
    pub enable_hit_errors: bool,
    pub current_checksum: String,
    #[cfg(feature = "pp")]
    pub cached_beatmap: Option<rosu_pp::Beatmap>,
    /// The file-metadata snapshot behind `current_checksum`: everything the
    /// `.osu` file pass resolved, which later ticks restore from instead of
    /// re-reading the file.
    ///
    /// This is the **session's** cache, not a per-client one, and it used to have
    /// a byte-identical twin on `CachedClientState` that nothing ever wrote --
    /// so a lookup on the client copy silently answered `None` forever.
    pub cached_metadata: Option<crate::beatmap::BeatmapSnapshot>,
    pub cached_stats: crate::beatmap::BeatmapStats,
    pub cached_stats_by_mods: HashMap<u32, crate::beatmap::BeatmapStats>,
    pub cached_accuracy: crate::v2::PerformanceAccuracy,
    pub cached_graph: crate::v2::PrecomputedGraph,
}

impl TournamentSession {
    /// How many osu! processes are currently attached.
    ///
    /// The tournament session's answer to the same question the solo session
    /// answers with its `attached` flag: the not-running contract is a property
    /// of the process handles, so with no clients there is nothing for the
    /// `/json*` routes to serve and tosu's `500` is the right answer.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn new(
        profile_name: &str,
        pointer_width: Option<usize>,
        scan_limit_bytes: usize,
    ) -> Result<Self> {
        let profile = load_profile(profile_name)?;
        let width = pointer_width.or((profile.pointer_width > 0).then_some(profile.pointer_width));
        Ok(Self {
            profile,
            pointer_width: width,
            scan_limit_bytes,
            clients: BTreeMap::new(),
            last_proc_scan: Instant::now() - std::time::Duration::from_secs(10),
            enable_chat: true,
            enable_pp: true,
            enable_hit_errors: true,
            current_checksum: String::new(),
            #[cfg(feature = "pp")]
            cached_beatmap: None,
            cached_metadata: None,
            cached_stats: crate::beatmap::BeatmapStats::default(),
            cached_stats_by_mods: HashMap::new(),
            cached_accuracy: crate::v2::PerformanceAccuracy::default(),
            cached_graph: crate::v2::PrecomputedGraph::default(),
        })
    }

    pub fn poll(&mut self) -> Result<TournamentSnapshot> {
        crate::instr_scope!(TourneyPoll);
        let start = Instant::now();

        // 1. Enumerate current osu processes and clean up dead ones periodically (every 1.5s) or if empty
        if self.clients.is_empty()
            || self.last_proc_scan.elapsed() >= std::time::Duration::from_millis(1500)
        {
            self.last_proc_scan = Instant::now();
            self.clients.retain(|_, client| client.memory.is_alive());
            let running_processes = list_processes(Some("osu!.exe")).unwrap_or_default();
            let running_pids: HashMap<u32, String> = running_processes
                .into_iter()
                .map(|p| (p.pid, p.name))
                .collect();

            self.clients.retain(|pid, _| running_pids.contains_key(pid));

            let pids_to_init: Vec<u32> = running_pids
                .keys()
                .copied()
                .filter(|pid| !self.clients.contains_key(pid))
                .collect();

            if !pids_to_init.is_empty() {
                let worker_count = pids_to_init.len().clamp(1, 8);
                let next = AtomicUsize::new(0);
                let initialized = Mutex::new(Vec::new());

                // Captured by the worker closures instead of `&self`: a client
                // state holds a `GradualCursor`, which is `Send` and not `Sync`,
                // so `&self` in a scoped closure would fail to compile. These
                // three are all `Sync` and are all `init_process_with` needs.
                let profile = self.profile.clone();
                let pointer_width = self.pointer_width;
                let scan_limit_bytes = self.scan_limit_bytes;

                std::thread::scope(|scope| {
                    for _ in 0..worker_count {
                        let next = &next;
                        let pids = &pids_to_init;
                        let initialized = &initialized;
                        let profile = &profile;
                        scope.spawn(move || {
                            loop {
                                let idx = next.fetch_add(1, Ordering::Relaxed);
                                if idx >= pids.len() {
                                    break;
                                }
                                let pid = pids[idx];
                                match TournamentSession::init_process_with(
                                    pid,
                                    profile,
                                    pointer_width,
                                    scan_limit_bytes,
                                ) {
                                    Ok(state) => initialized
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .push((pid, state)),
                                    Err(e) => tracing::warn!("Process {pid} init error: {e}"),
                                }
                            }
                        });
                    }
                });

                for (pid, state) in initialized
                    .into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                {
                    self.clients.insert(pid, state);
                }
            }
        }

        let mut spectator_ids: Vec<(u32, usize)> = self
            .clients
            .values()
            .filter_map(|client| client.ipc_id.map(|ipc| (client.pid, ipc)))
            .collect();
        spectator_ids.sort_by_key(|(_, ipc)| *ipc);
        let team_cutoff = (spectator_ids.len() + 1) / 2;
        let team_by_pid: HashMap<u32, String> = spectator_ids
            .iter()
            .enumerate()
            .map(|(rank, (pid, _))| {
                (
                    *pid,
                    if rank < team_cutoff { "left" } else { "right" }.to_string(),
                )
            })
            .collect();

        // 5. Read all clients and manager state with high-speed direct memory dereferences
        let mut manager_state: Option<TournamentState> = None;
        let mut root_profile: Option<LocalProfile> = None;
        let mut root_beatmap: Option<BeatmapSnapshot> = None;
        let mut game_folder = String::new();
        let mut songs_folder = String::new();
        let mut skin_folder = String::new();
        let mut game_time = 0;
        let mut spectator_views = Vec::new();
        let mut spectator_teams = HashMap::new();

        // Pre-pass: map spectator usernames to their assigned team for chat mapping
        for client in self.clients.values_mut() {
            if client.ipc_id.is_some() {
                let team = team_by_pid
                    .get(&client.pid)
                    .cloned()
                    .unwrap_or_else(|| "right".to_string());
                if let Some(user_pat) = client.spectating_user_pattern_addr {
                    if let Ok(user_addr) = client.memory.read_indirect_pointer(user_pat) {
                        if user_addr != 0 {
                            if user_addr == client.cached_user_ptr && client.cached_user.is_some() {
                                if let Some(ref u) = client.cached_user {
                                    spectator_teams.insert(u.name.clone(), team);
                                }
                            } else {
                                client.cached_user_ptr = user_addr;
                                if let Ok(user) = read_tournament_user(&client.memory, user_addr) {
                                    spectator_teams.insert(user.name.clone(), team);
                                    client.cached_user = Some(user);
                                }
                            }
                        }
                    }
                }
            }
        }

        for client in self.clients.values_mut() {
            let mut beatmap = if let Some(base_addr) = client.base_pattern_addr {
                if let Ok(beatmap_addr) =
                    crate::beatmap::read_beatmap_ptr(&client.memory, base_addr)
                {
                    if beatmap_addr == 0 {
                        client.cached_beatmap_ptr = 0;
                        client.cached_beatmap_id = 0;
                        client.cached_beatmap_snapshot = None;
                        None
                    } else {
                        let live_time = crate::beatmap::read_live_time(
                            &client.memory,
                            client.play_time_pattern_addr,
                        );
                        let live_id = crate::beatmap::read_beatmap_id(&client.memory, beatmap_addr);
                        let reuse = client.cached_beatmap_snapshot.is_some()
                            && !crate::beatmap::beatmap_refresh_needed(
                                beatmap_addr,
                                client.cached_beatmap_ptr,
                                live_id,
                                client.cached_beatmap_id,
                            );
                        if reuse && let Some(mut bm) = client.cached_beatmap_snapshot.clone() {
                            bm.time.live = live_time;
                            Some(bm)
                        } else {
                            client.cached_beatmap_ptr = beatmap_addr;
                            if let Ok(bm) = crate::beatmap::read_beatmap_from_ptr(
                                &client.memory,
                                beatmap_addr,
                                base_addr,
                                live_time,
                                self.pointer_width.unwrap_or(4),
                            ) {
                                if bm.id > 0 || !bm.title.is_empty() {
                                    // Caching the id only alongside a snapshot that
                                    // carries an id or a title: a partial read with no
                                    // id would pin the cache to 0 and the real map
                                    // would never be picked up.
                                    client.cached_beatmap_id = bm.id;
                                    client.cached_beatmap_snapshot = Some(bm.clone());
                                    Some(bm)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(audio_addr) = client.audio_length_pattern_addr
                && let Some(beatmap) = beatmap.as_mut()
                && let Ok(audio_ptr) = client.memory.read_indirect_pointer(audio_addr)
                && let Ok(audio_length) = client.memory.read_f64(audio_ptr.saturating_add(4))
            {
                beatmap.time.mp3_length = audio_length as i32;
            }
            if let Some(beatmap_ref) = beatmap.as_ref() {
                let osu_path = std::path::Path::new(&client.songs_folder)
                    .join(&beatmap_ref.folder)
                    .join(&beatmap_ref.filename);
                if client.current_checksum != beatmap_ref.checksum {
                    client.current_checksum = beatmap_ref.checksum.clone();
                    client.cached_gameplay = None;
                    client.cached_total_hits = 0;
                    client.cached_hit_errors = Arc::default();
                    client.cached_unstable_rate = 0.0;
                    // The rating is for the old map. `cached_total_hits` is
                    // already back to zero, so a fresh play that has not hit
                    // anything yet would otherwise find the pre-change key
                    // waiting for it and republish the previous map's numbers.
                    #[cfg(feature = "pp")]
                    {
                        client.cached_live_pp = None;
                    }
                }
                if beatmap_ref.checksum != self.current_checksum && !beatmap_ref.checksum.is_empty()
                {
                    self.current_checksum = beatmap_ref.checksum.clone();
                    self.cached_stats_by_mods.clear();
                    if let Some(beatmap_mut) = beatmap.as_mut() {
                        // Same contract as the solo path: the game's current
                        // ruleset is what `mode.number` holds until the file's
                        // own ruleset replaces it.
                        let current_ruleset = beatmap_mut.mode.number;
                        if crate::beatmap::populate_beatmap_file_metadata(beatmap_mut, &osu_path) {
                            crate::beatmap::apply_beatmap_ruleset(beatmap_mut, current_ruleset);
                        }
                        self.cached_metadata = Some(beatmap_mut.clone());
                    }
                    #[cfg(feature = "pp")]
                    if let Ok(bytes) = std::fs::read(&osu_path) {
                        if let Ok(map) = rosu_pp::Beatmap::from_bytes(&bytes) {
                            let mods_legacy = crate::pp::calculator::parse_mods_bits(0);
                            let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(&map);
                            let mut temp_snap = beatmap.as_ref().cloned().unwrap_or_default();
                            crate::beatmap::populate_beatmap_statistics_with_diff(
                                &mut temp_snap,
                                &map,
                                &diff,
                                0,
                            );
                            temp_snap.stats.stars.live = 0.0;
                            self.cached_stats = temp_snap.stats;
                            self.cached_accuracy =
                                crate::pp::calculator::calc_accuracy_table_from_diff(&diff);
                            let first_obj =
                                beatmap.as_ref().map_or(temp_snap.time.first_object, |b| {
                                    if b.time.first_object > 0 {
                                        b.time.first_object
                                    } else {
                                        temp_snap.time.first_object
                                    }
                                });
                            let last_obj =
                                beatmap.as_ref().map_or(temp_snap.time.last_object, |b| {
                                    if b.time.last_object > 0 {
                                        b.time.last_object
                                    } else {
                                        temp_snap.time.last_object
                                    }
                                });
                            let mp3_len = beatmap.as_ref().map_or(temp_snap.time.mp3_length, |b| {
                                if b.time.mp3_length > 0 {
                                    b.time.mp3_length
                                } else {
                                    temp_snap.time.mp3_length
                                }
                            });
                            self.cached_graph =
                                performance_graph(&map, 0, first_obj, last_obj, mp3_len);
                            self.cached_beatmap = Some(map);
                        }
                    }
                } else if let (Some(beatmap_mut), Some(meta)) =
                    (beatmap.as_mut(), self.cached_metadata.as_ref())
                {
                    beatmap_mut.source = meta.source.clone();
                    beatmap_mut.tags = meta.tags.clone();
                    beatmap_mut.stats.objects = meta.stats.objects.clone();
                    beatmap_mut.time.first_object = meta.time.first_object;
                    beatmap_mut.time.last_object = meta.time.last_object;
                    if beatmap_mut.time.mp3_length == 0 {
                        beatmap_mut.time.mp3_length = meta.time.mp3_length;
                    }
                    beatmap_mut.stats.bpm = meta.stats.bpm.clone();
                    restore_beatmap_ruleset(beatmap_mut, meta);
                }
                #[cfg(feature = "pp")]
                if let (Some(beatmap_mut), Some(map)) =
                    (beatmap.as_mut(), self.cached_beatmap.as_ref())
                {
                    beatmap_mut.stats = self.cached_stats.clone();
                    let live = beatmap_mut.time.live as f64;
                    beatmap_mut.is_kiai = map
                        .effect_points
                        .iter()
                        .rev()
                        .find(|ep| ep.time <= live)
                        .map_or(false, |ep| ep.kiai);
                    beatmap_mut.is_break = map
                        .breaks
                        .iter()
                        .any(|b| live >= b.start_time && live <= b.end_time);
                }
            }
            let need_ruleset = client.ruleset_container_addr.is_none();
            let need_user =
                client.ipc_id.is_some() && client.spectating_user_pattern_addr.is_none();
            if (need_ruleset || need_user)
                && client.last_pattern_retry.elapsed() >= std::time::Duration::from_millis(2000)
            {
                client.last_pattern_retry = Instant::now();
                if need_ruleset {
                    client.ruleset_container_addr = crate::client::resolve_ruleset_container(
                        &client.memory,
                        &self.profile,
                        self.scan_limit_bytes,
                    )
                    .ok();
                }
                if need_user {
                    if let Ok((user_pat_src, user_pat_off)) =
                        self.profile.pattern("spectating_user_ptr")
                    {
                        if let Ok(user_pat) = BytePattern::parse(user_pat_src) {
                            if let Ok(matches) = client.memory.scan_pattern(
                                &user_pat,
                                None,
                                1,
                                self.scan_limit_bytes,
                            ) {
                                if let Some(&first) = matches.first() {
                                    if let Ok(addr) = checked_add_signed(first, user_pat_off) {
                                        client.spectating_user_pattern_addr = Some(addr);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let ruleset_addr = match client.ruleset_container_addr {
                Some(container_addr) => match client.memory.read_pointer(container_addr) {
                    Ok(container) if container != 0 => {
                        match client.memory.read_pointer(container.saturating_add(4)) {
                            Ok(ruleset) if ruleset != 0 => Some(ruleset),
                            _ => None,
                        }
                    }
                    _ => None,
                },
                None => None,
            };

            if client.is_manager || client.ipc_id.is_none() {
                if let Some(ruleset_addr) = ruleset_addr
                    && let Ok(mut tourney) = read_tournament_state(&client.memory, ruleset_addr)
                {
                    client.is_manager = true;
                    if self.enable_chat {
                        if let Some(chat_pat) = client.chat_engine_pattern_addr {
                            let cached_chat = client.cached_chat.as_slice();
                            let cached = match client.cached_chat_key.as_ref() {
                                Some(key) if !cached_chat.is_empty() => Some((key, cached_chat)),
                                _ => None,
                            };
                            if let Ok(chat) = read_tournament_chat(
                                &client.memory,
                                chat_pat,
                                &spectator_teams,
                                cached,
                            ) {
                                client.cached_chat_key = Some(chat.key);
                                client.cached_chat = chat.messages.clone();
                                tourney.chat = chat.messages;
                            }
                        }
                    }
                    manager_state = Some(tourney);
                    if game_folder.is_empty() {
                        game_folder = client.game_folder.clone();
                        songs_folder = client.songs_folder.clone();
                    }
                    if skin_folder.is_empty()
                        && let Some(skin_addr) = client.skin_pattern_addr
                    {
                        skin_folder =
                            read_skin_folder(&client.memory, skin_addr).unwrap_or_default();
                    }
                    if game_time == 0
                        && let Some(game_time_addr) = client.game_time_pattern_addr
                        && let Ok(value) = client.memory.read_indirect_pointer(game_time_addr)
                    {
                        game_time = value as i32;
                    }
                    if root_profile.is_none() {
                        root_profile = client
                            .user_profile_pattern_addr
                            .zip(client.raw_login_status_pattern_addr)
                            .and_then(|(user_pattern, login_pattern)| {
                                read_local_profile(&client.memory, user_pattern, login_pattern).ok()
                            });
                    }
                    if root_beatmap.is_none() {
                        root_beatmap = beatmap.clone();
                    }
                }
                continue;
            }

            if let Some(ipc_id) = client.ipc_id {
                let team = team_by_pid
                    .get(&client.pid)
                    .cloned()
                    .unwrap_or_else(|| "right".to_string());

                let user = if let Some(user_pat) = client.spectating_user_pattern_addr {
                    if let Ok(user_addr) = client.memory.read_indirect_pointer(user_pat) {
                        if user_addr != 0
                            && user_addr == client.cached_user_ptr
                            && client.cached_user.is_some()
                        {
                            client.cached_user.clone()
                        } else if user_addr != 0 {
                            client.cached_user_ptr = user_addr;
                            let u = read_tournament_user(&client.memory, user_addr).ok();
                            client.cached_user = u.clone();
                            u
                        } else {
                            client.cached_user_ptr = 0;
                            client.cached_user = None;
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let mut gameplay = ruleset_addr.and_then(|ruleset| {
                    let cached = Some((
                        client.cached_total_hits,
                        &client.cached_hit_errors,
                        client.cached_unstable_rate,
                    ));
                    // `grade_max` needs `menu.objectCount`, which
                    // `read_beatmap_from_ptr` already stores as
                    // `beatmap.stats.objects.total` from `beatmap_addr + 0xF8`
                    // (`memory/stable.ts:959`) -- the same read the solo path
                    // uses. Taking it from the beatmap this tick already read
                    // removes a cache lookup that was reading the wrong field
                    // entirely: it asked `client.cached_metadata`, a
                    // per-client field that nothing ever writes, so
                    // `object_count` was always `0`, `remaining` was always
                    // `<= 0`, and the projection collapsed to the current grade
                    // for every client in a tournament. With no beatmap read
                    // this tick there is no object count, and `0` is the
                    // conservative answer the projection documents.
                    let object_count = beatmap.as_ref().map_or(0, |b| b.stats.objects.total);
                    crate::client::read_gameplay_state_cached(
                        &client.memory,
                        ruleset,
                        cached,
                        object_count,
                    )
                    .ok()
                });
                if let Some(ref mut g) = gameplay {
                    let total_hits = (g.hit_300 + g.hit_100 + g.hit_50 + g.hit_miss) as u32;
                    client.cached_total_hits = total_hits;
                    client.cached_unstable_rate = g.unstable_rate;
                    if self.enable_hit_errors {
                        client.cached_hit_errors = Arc::clone(&g.hit_error_array);
                    } else {
                        client.cached_hit_errors = Arc::default();
                        g.hit_error_array = Arc::default();
                    }
                    client.cached_gameplay = Some(g.clone());
                } else if client.cached_gameplay.is_some() {
                    gameplay = client.cached_gameplay.clone();
                } else {
                    client.cached_total_hits = 0;
                    client.cached_hit_errors = Arc::default();
                    client.cached_unstable_rate = 0.0;
                }
                #[cfg(feature = "pp")]
                if self.enable_pp {
                    if let (Some(beatmap), Some(map), Some(g)) = (
                        beatmap.as_mut(),
                        self.cached_beatmap.as_ref(),
                        gameplay.as_ref(),
                    ) {
                        let diff_mods = g.mods & !mod_bits::SCORE_V2;
                        if diff_mods == 0 {
                            beatmap.stats = self.cached_stats.clone();
                        } else if let Some(stats) = self.cached_stats_by_mods.get(&diff_mods) {
                            beatmap.stats = stats.clone();
                        } else {
                            let mut temp = self
                                .cached_metadata
                                .clone()
                                .unwrap_or_else(|| beatmap.clone());
                            crate::beatmap::populate_beatmap_statistics(&mut temp, map, diff_mods);
                            let stats = temp.stats;
                            self.cached_stats_by_mods.insert(diff_mods, stats.clone());
                            beatmap.stats = stats;
                        }
                    }
                }
                #[cfg(feature = "pp")]
                let live_pp = if self.enable_pp {
                    if let Some(map) = self.cached_beatmap.as_ref() {
                        // The raw mod bits, kept alongside the parsed form: the
                        // rating depends on the mods, so they belong in the
                        // cache key below. `parse_mods_bits(0)` is the default,
                        // so the no-gameplay case needs no separate arm.
                        let mods_bits = gameplay.as_ref().map(|g| g.mods).unwrap_or(0);
                        let mods_legacy = crate::pp::calculator::parse_mods_bits(mods_bits);
                        let (combo, n300, n100, n50, n0) = gameplay
                            .as_ref()
                            .map(|g| {
                                (
                                    g.combo as u32,
                                    g.hit_300 as u32,
                                    g.hit_100 as u32,
                                    g.hit_50 as u32,
                                    g.hit_miss as u32,
                                )
                            })
                            .unwrap_or((0, 0, 0, 0, 0));
                        let total_hits = n300 + n100 + n50 + n0;
                        let map_id = beatmap.as_ref().map_or(0, |b| b.id as u32);
                        // The gate. The rating depends only on the map, the mods
                        // and the six judgement numbers, so a tick that saw no new
                        // judgement cannot change it -- and a tick that is not a
                        // play at all (the client is in the lobby) is the common
                        // case, where all six are zero and the answer is the
                        // same zeroed rating every time.
                        //
                        // The tuple carries the hit total as well, because that is
                        // what the cursor is advanced by: a set of counts that
                        // summed differently would mean a different point on the
                        // curve even if the individual numbers matched. The raw
                        // mod bits are in it for the same reason -- a mod change
                        // alters the rating without altering a single counter, so
                        // keying on the judgements alone would serve a rating
                        // computed under different mods.
                        let pp_key = (mods_bits, combo, n300, n100, n50, n0, total_hits);
                        let cached = client.cached_live_pp.as_ref();
                        let live_pp = match cached {
                            Some((key, pp)) if *key == pp_key => Some(pp.clone()),
                            _ => {
                                // `full_difficulty` rather than a session field:
                                // the loop holds `&mut self.clients`, so a
                                // `&mut self` accessor is unreachable from in here.
                                // The cache makes it once per (map, mods) for the
                                // whole process, not once per client.
                                let full = crate::pp::calculator::full_difficulty(
                                    map_id,
                                    map,
                                    mods_legacy,
                                );
                                // The curve belongs to the client, not the
                                // session: each one is at a different point in the
                                // same map, and a shared forward-only cursor would
                                // make the ones that are behind rebuild it on
                                // every tick.
                                let live_attrs = client.gradual_cursor.advance_to(
                                    map_id,
                                    map,
                                    mods_legacy,
                                    total_hits,
                                );
                                let live_stars = live_attrs
                                    .map(crate::pp::calculator::live_stars)
                                    .unwrap_or(0.0);
                                if let Some(b) = beatmap.as_mut() {
                                    b.stats.stars.live = live_stars;
                                }
                                let pp = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                                    live_attrs,
                                    &full,
                                    mods_legacy,
                                    combo,
                                    n300,
                                    n100,
                                    n50,
                                    n0,
                                );
                                client.cached_live_pp = Some((pp_key, pp.clone()));
                                Some(pp)
                            }
                        };
                        live_pp
                    } else {
                        None
                    }
                } else {
                    None
                };
                #[cfg(not(feature = "pp"))]
                let live_pp: Option<crate::pp::LivePpResult> = None;

                let is_playing = gameplay.as_ref().map_or(false, |g| {
                    g.combo > 0 || (g.hit_300 + g.hit_100 + g.hit_50 + g.hit_miss) > 0
                });
                if !is_playing {
                    if let Some(b) = beatmap.as_mut() {
                        b.stats.stars.live = 0.0;
                    }
                }
                if root_beatmap.is_none() {
                    root_beatmap = beatmap.clone();
                }

                spectator_views.push(TournamentClientView {
                    pid: client.pid,
                    ipc_id,
                    team,
                    user,
                    gameplay,
                    beatmap,
                    pp: live_pp,
                    error: None,
                });
            }
        }

        // Sort spectator views strictly by ipc_id
        spectator_views.sort_by_key(|c| c.ipc_id);

        if root_beatmap.is_none() {
            root_beatmap = spectator_views.first().and_then(|v| v.beatmap.clone());
        }

        // Propagate ranked status from manager beatmap (valid status range: 1..=7)
        let manager_status = self
            .clients
            .values()
            .find(|c| c.is_manager || c.ipc_id.is_none())
            .and_then(|c| c.cached_beatmap_snapshot.as_ref())
            .map(|b| b.status.clone())
            .filter(|s| (1..=7).contains(&s.number));

        let resolved_status = manager_status.or_else(|| {
            root_beatmap
                .as_ref()
                .map(|b| b.status.clone())
                .filter(|s| (1..=7).contains(&s.number))
                .or_else(|| {
                    self.clients
                        .values()
                        .filter_map(|c| c.cached_beatmap_snapshot.as_ref())
                        .map(|b| b.status.clone())
                        .find(|s| (1..=7).contains(&s.number))
                })
        });

        if let Some(status) = resolved_status {
            if let Some(b) = root_beatmap.as_mut() {
                if b.status.number == 0 {
                    b.status = status.clone();
                }
            }
            for view in &mut spectator_views {
                if let Some(b) = view.beatmap.as_mut() {
                    if b.status.number == 0 {
                        b.status = status.clone();
                    }
                }
            }
        }
        if let Some(b) = root_beatmap.as_mut() {
            if b.time.live == 0 {
                if let Some(live) = spectator_views
                    .iter()
                    .find_map(|v| v.beatmap.as_ref().map(|bm| bm.time.live).filter(|&l| l > 0))
                {
                    b.time.live = live;
                    #[cfg(feature = "pp")]
                    if let Some(map) = self.cached_beatmap.as_ref() {
                        let live_f = live as f64;
                        b.is_kiai = map
                            .effect_points
                            .iter()
                            .rev()
                            .find(|ep| ep.time <= live_f)
                            .map_or(false, |ep| ep.kiai);
                        b.is_break = map.breaks.iter().any(|b_break| {
                            live_f >= b_break.start_time && live_f <= b_break.end_time
                        });
                    }
                }
            }
            b.stats.stars.live = 0.0;
        }

        let focused = {
            #[cfg(target_os = "windows")]
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetForegroundWindow, GetWindowThreadProcessId,
                };
                let hwnd = GetForegroundWindow();
                if !hwnd.is_null() {
                    let mut fg_pid = 0u32;
                    GetWindowThreadProcessId(hwnd, &mut fg_pid);
                    self.clients.contains_key(&fg_pid)
                } else {
                    false
                }
            }
            #[cfg(not(target_os = "windows"))]
            self.clients.values().any(|c| c.memory.is_foreground())
        };

        if game_folder.is_empty() {
            if let Some(client) = self
                .clients
                .values()
                .find(|client| !client.game_folder.is_empty())
            {
                game_folder = client.game_folder.clone();
                songs_folder = client.songs_folder.clone();
            }
        }
        if game_time == 0
            && let Some(client) = self
                .clients
                .values()
                .find(|client| client.game_time_pattern_addr.is_some())
            && let Some(addr) = client.game_time_pattern_addr
            && let Ok(value) = client.memory.read_indirect_pointer(addr)
        {
            game_time = value as i32;
        }

        let captured_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| anyhow::anyhow!("system clock is before Unix epoch: {error}"))?
            .as_millis();

        let poll_duration_us = start.elapsed().as_micros();

        Ok(TournamentSnapshot {
            captured_at_ms,
            poll_duration_us,
            manager: manager_state,
            profile: root_profile,
            beatmap: root_beatmap,
            game_folder,
            songs_folder,
            skin_folder,
            game_time,
            clients: spectator_views,
            performance: crate::v2::PerformanceState {
                accuracy: self.cached_accuracy.clone(),
                graph: self.cached_graph.clone(),
            },
            focused,
        })
    }

    /// Build the cached state for one tournament client process.
    ///
    /// An associated function taking its inputs rather than a `&self` method, and
    /// that is the point. `TournamentSession::poll` builds client states on a
    /// pool of scoped threads, and a scoped closure has to be `Send`, so a
    /// `&self` capture would require `TournamentSession: Sync` -- and with it
    /// every field. A client state carries a `GradualCursor`, which is `Send`
    /// but deliberately not `Sync` (see the `unsafe impl` on it in
    /// `pp::calculator`), so the session can no longer be `Sync` now that a play
    /// can hold a live curve. Everything needed here is `Sync`, so passing it in
    /// keeps the scope off the session entirely and the assertion down to one
    /// `Send`.
    #[allow(clippy::too_many_arguments)]
    fn init_process_with(
        pid: u32,
        profile: &crate::profile::ClientProfile,
        pointer_width: Option<usize>,
        scan_limit_bytes: usize,
    ) -> Result<CachedClientState> {
        let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
        let command_line = memory.command_line().unwrap_or_default();
        let spectate_info = parse_spectate_client_arg(&command_line);
        let ipc_id = spectate_info.map(|(id, _)| id);
        let is_spectator = ipc_id.is_some();
        let is_manager = is_tournament_manager_cmd(&command_line);

        // Scan rulesets_addr pattern to find container pointer address
        let ruleset_container_addr =
            crate::client::resolve_ruleset_container(&memory, profile, scan_limit_bytes).ok();

        // If spectator, scan spectating_user_ptr
        let mut spectating_user_pattern_addr = None;
        if is_spectator || !is_manager {
            if let Ok((user_pat_src, user_pat_off)) = profile.pattern("spectating_user_ptr") {
                if let Ok(user_pat) = BytePattern::parse(user_pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&user_pat, None, 1, scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, user_pat_off) {
                                spectating_user_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }

        // If manager, scan tournament_chat_engine
        let mut chat_engine_pattern_addr = None;
        if is_manager || !is_spectator {
            if let Ok((chat_pat_src, chat_pat_off)) = profile.pattern("tournament_chat_engine") {
                if let Ok(chat_pat) = BytePattern::parse(chat_pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&chat_pat, None, 1, scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, chat_pat_off) {
                                chat_engine_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }
        let base_pattern_addr = profile
            .pattern("base_addr")
            .ok()
            .and_then(|(source, offset)| {
                find_pattern(&memory, source, offset, scan_limit_bytes).ok()
            });
        let play_time_pattern_addr =
            profile
                .pattern("play_time_addr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let audio_length_pattern_addr =
            profile
                .pattern("get_audio_length_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let game_time_pattern_addr =
            profile
                .pattern("game_time_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let skin_pattern_addr =
            profile
                .pattern("skin_data_addr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let user_profile_pattern_addr =
            profile
                .pattern("user_profile_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let raw_login_status_pattern_addr =
            profile
                .pattern("raw_login_status_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, scan_limit_bytes).ok()
                });
        let (game_folder, songs_folder) = memory
            .process_image_path()
            .ok()
            .and_then(|path| {
                let parent = std::path::Path::new(&path).parent()?;
                Some((
                    parent.to_string_lossy().to_string(),
                    parent.join("Songs").to_string_lossy().to_string(),
                ))
            })
            .unwrap_or_default();

        Ok(CachedClientState {
            pid,
            memory,
            command_line,
            ipc_id,
            is_manager,
            is_spectator,
            ruleset_container_addr,
            spectating_user_pattern_addr,
            chat_engine_pattern_addr,
            base_pattern_addr,
            play_time_pattern_addr,
            audio_length_pattern_addr,
            game_time_pattern_addr,
            skin_pattern_addr,
            user_profile_pattern_addr,
            raw_login_status_pattern_addr,
            game_folder,
            songs_folder,
            current_checksum: String::new(),
            #[cfg(feature = "pp")]
            cached_beatmap: None,
            cached_total_hits: 0,
            #[cfg(feature = "pp")]
            cached_live_pp: None,
            cached_hit_errors: Arc::default(),
            cached_unstable_rate: 0.0,
            cached_gameplay: None,
            #[cfg(feature = "pp")]
            gradual_cursor: crate::pp::calculator::GradualCursor::new(),
            cached_beatmap_ptr: 0,
            cached_beatmap_id: 0,
            cached_beatmap_snapshot: None,
            cached_user_ptr: 0,
            cached_user: None,
            cached_chat_key: None,
            cached_chat: Vec::new(),
            last_pattern_retry: Instant::now(),
        })
    }
}

#[cfg(feature = "pp")]
#[allow(dead_code)]
fn performance_accuracy(map: &rosu_pp::Beatmap, mods: u32) -> crate::v2::PerformanceAccuracy {
    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
    crate::pp::calculator::calc_accuracy_table_from_diff(&diff)
}

#[cfg(feature = "pp")]
fn performance_graph(
    map: &rosu_pp::Beatmap,
    mods: u32,
    first_object: i32,
    _last_object: i32,
    mp3_length: i32,
) -> crate::v2::PrecomputedGraph {
    let clock_rate: f64 = if (mods & mod_bits::DT) != 0 || (mods & mod_bits::NC) != 0 {
        1.5
    } else if (mods & mod_bits::HT) != 0 {
        0.75
    } else {
        1.0
    };

    let first_obj_time = if first_object > 0 {
        first_object as f64
    } else {
        map.hit_objects.first().map_or(0.0, |o| o.start_time)
    };
    let start = first_obj_time / clock_rate;
    let mp3_time = if mp3_length > 0 {
        mp3_length as f64
    } else {
        map.hit_objects
            .last()
            .map_or(first_obj_time, |o| o.start_time)
    };
    let total_time = mp3_time / clock_rate;

    let empty_offset_l = (start / 400.0).floor().max(0.0) as usize;

    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let strains = rosu_pp::Difficulty::new().mods(mods_legacy).strains(map);

    let mut aim = Vec::new();
    let mut aim_no_sliders = Vec::new();
    let mut speed = Vec::new();
    let mut flashlight = Vec::new();
    let mut reading = Vec::new();
    let has_flashlight_mod = (mods & mod_bits::FL) != 0;

    let mut strain_count = 0;
    if let rosu_pp::any::Strains::Osu(values) = strains {
        strain_count = values.aim.len();
        aim = values.aim;
        aim_no_sliders = values.aim_no_sliders;
        speed = values.speed;
        reading = values.reading;
        if has_flashlight_mod {
            flashlight = values.flashlight;
        }
    }

    let last_strain_time = start + (strain_count.saturating_sub(1) as f64) * 400.0;
    let empty_offset_r = if total_time >= last_strain_time {
        ((total_time - last_strain_time) / 400.0).floor().max(0.0) as usize
    } else {
        0
    };

    let total_points = empty_offset_l + strain_count + empty_offset_r;
    let mut xaxis = Vec::with_capacity(total_points);
    for ind in 0..empty_offset_l {
        xaxis.push(ind as f64 * 400.0);
    }
    for ind in 0..(strain_count + empty_offset_r) {
        xaxis.push(start + ind as f64 * 400.0);
    }

    let pad_series = |values: Vec<f64>| -> Vec<f64> {
        let mut result = Vec::with_capacity(total_points);
        result.resize(empty_offset_l, -100.0);
        result.extend(values);
        result.resize(total_points, -100.0);
        result
    };

    let flashlight_data = if has_flashlight_mod {
        pad_series(flashlight)
    } else {
        vec![-100.0; empty_offset_l + empty_offset_r]
    };

    let graph = crate::v2::PerformanceGraph {
        series: vec![
            crate::v2::GraphSeries {
                name: "aim".to_string(),
                data: pad_series(aim),
            },
            crate::v2::GraphSeries {
                name: "aimNoSliders".to_string(),
                data: pad_series(aim_no_sliders),
            },
            crate::v2::GraphSeries {
                // A real series as of `rosu-pp-gemini` 5.0.3, which added
                // `OsuStrains::reading` -- the osu!std reading skill tracks fixed
                // 400 ms sections the same way `speed` and `flashlight` do, so it
                // indexes this x-axis without reshaping.
                //
                // It was `[]` before, for a good reason that no longer applies: the
                // skill computed no sections at all, and a flat 0.0 line is a claim
                // about the map that is indistinguishable from a real reading value.
                // Zero-filling and cloning the aim series (the 1.0.4 defect) are
                // both worse than either -- reading is a distinct skill, so a clone
                // is a wrong value rather than an absent one.
                //
                // These are rosu-pp's numbers, not tosu's: tosu reads them from its
                // own lazer calculator fork (audit-1.0.5.md `G-08`). The shape,
                // section count and magnitude should agree; the values will not be
                // identical.
                name: "reading".to_string(),
                data: pad_series(reading),
            },
            crate::v2::GraphSeries {
                name: "flashlight".to_string(),
                data: flashlight_data,
            },
            crate::v2::GraphSeries {
                name: "speed".to_string(),
                data: pad_series(speed),
            },
        ],
        xaxis,
    };
    crate::v2::PrecomputedGraph::new(&graph)
}

fn read_skin_folder(memory: &ProcessMemory, pattern_addr: u64) -> Result<String> {
    let skin_osu_addr = memory
        .read_i32(pattern_addr.saturating_add(7))
        .context("reading skin osu address")?;
    if skin_osu_addr == 0 {
        return Ok(String::new());
    }
    let skin_osu_base = memory
        .read_pointer(skin_osu_addr as u64)
        .context("reading skin base")?;
    if skin_osu_base == 0 {
        return Ok(String::new());
    }
    let skin_string = memory
        .read_pointer(skin_osu_base.saturating_add(0x44))
        .context("reading skin string")?;
    crate::beatmap::read_sharp_string_ptr(memory, skin_string)
}

fn join_beatmap_path(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.to_string()
    } else if file.is_empty() {
        folder.to_string()
    } else {
        format!("{}\\{}", folder, file)
    }
}

fn guest_profile_state() -> crate::v2::ProfileState {
    crate::v2::ProfileState {
        user_status: crate::v2::OsuStatusState {
            number: 256,
            name: "guest".to_string(),
        },
        bancho_status: crate::v2::OsuStatusState {
            number: 0,
            name: "idle".to_string(),
        },
        id: -1,
        name: "Guest".to_string(),
        mode: crate::v2::OsuStatusState {
            number: 0,
            name: "osu".to_string(),
        },
        background_colour: "ff010101".to_string(),
        ..Default::default()
    }
}

fn ruleset_name(value: i32) -> &'static str {
    crate::reader::ruleset_name(value)
}

fn profile_state_from_local(profile: &LocalProfile) -> crate::v2::ProfileState {
    crate::v2::ProfileState {
        user_status: crate::v2::OsuStatusState {
            number: profile.raw_login_status,
            // The three name tables are the ones in `reader`, transcribed from
            // `common/enums/osu.ts`. They used to be inlined here as byte-equal
            // duplicates, which is not a defect today and is exactly the shape
            // that lets a table be corrected in one place and not the other.
            name: crate::reader::login_status_name(profile.raw_login_status).to_string(),
        },
        bancho_status: crate::v2::OsuStatusState {
            number: profile.raw_bancho_status,
            name: crate::reader::bancho_status_name(profile.raw_bancho_status).to_string(),
        },
        id: profile.id,
        name: profile.name.clone(),
        mode: crate::v2::OsuStatusState {
            number: profile.play_mode,
            name: crate::reader::ruleset_name(profile.play_mode).to_string(),
        },
        ranked_score: profile.ranked_score,
        level: profile.level as f64,
        accuracy: profile.accuracy,
        pp: profile.performance_points,
        play_count: profile.play_count,
        global_rank: profile.rank,
        country_code: crate::v2::OsuStatusState {
            number: profile.country_code,
            name: country_code_name(profile.country_code).to_ascii_uppercase(),
        },
        background_colour: format!("{:x}", profile.background_colour),
        matchmaking: None,
    }
}

fn country_code_name(value: i32) -> &'static str {
    // The table lives in `reader` and is transcribed from tosu's `country.ts`.
    // This used to be a second, byte-identical copy of the same drifted string
    // literal, which is how a one-entry fix could have been applied to one copy
    // and not the other. One table, one function; see `reader::COUNTRY_CODES`.
    crate::reader::country_name(value)
}

pub struct SoloSession {
    profile: ClientProfile,
    pointer_width: usize,
    scan_limit_bytes: usize,
    pid: Option<u32>,
    memory: Option<ProcessMemory>,
    base_pattern_addr: Option<u64>,
    status_pattern_addr: Option<u64>,
    play_time_pattern_addr: Option<u64>,
    audio_length_pattern_addr: Option<u64>,
    game_time_pattern_addr: Option<u64>,
    skin_pattern_addr: Option<u64>,
    pub menu_mods_pattern_addr: Option<u64>,
    pub user_profile_pattern_addr: Option<u64>,
    pub raw_login_status_pattern_addr: Option<u64>,
    pub ruleset_container_addr: Option<u64>,
    game_folder: String,
    songs_folder: String,
    current_checksum: String,
    #[cfg(feature = "pp")]
    cached_beatmap: Option<rosu_pp::Beatmap>,
    #[cfg(feature = "pp")]
    cached_mods: u32,
    #[cfg(feature = "pp")]
    cached_difficulty_attrs: Option<rosu_pp::any::DifficultyAttributes>,
    #[cfg(feature = "pp")]
    cached_accuracy: crate::v2::PerformanceAccuracy,
    #[cfg(feature = "pp")]
    cached_graph: crate::v2::PrecomputedGraph,
    #[cfg(feature = "pp")]
    cached_gameplay_hits: (u32, u32, u32, u32, u32, u32),
    #[cfg(feature = "pp")]
    cached_live_pp: Option<crate::pp::LivePpResult>,
    #[cfg(feature = "pp")]
    cached_results_hits: (u32, u32, u32, u32, u32, u32),
    #[cfg(feature = "pp")]
    cached_results_pp: Option<crate::pp::LivePpResult>,
    /// (beatmap id, mods) the menu PP was last computed for, so it is only
    /// recalculated when the map or mods change.
    #[cfg(feature = "pp")]
    cached_idle_pp_key: Option<(u32, u32)>,
    /// The live difficulty curve for the current play, carried across ticks so
    /// each judgement only folds the objects it just passed. Rebuilt by itself
    /// when the map, the mods, or the direction of play changes.
    #[cfg(feature = "pp")]
    gradual_cursor: crate::pp::calculator::GradualCursor,
    cached_beatmap_metadata: crate::beatmap::BeatmapSnapshot,
    #[cfg(feature = "pp")]
    cached_stats: crate::beatmap::BeatmapStats,
    cached_beatmap_ptr: u64,
    /// Difficulty id of the map behind `cached_beatmap`, so a map swap that
    /// reuses the same beatmap object is still noticed.
    cached_beatmap_id: i32,
    last_skin_read: Instant,
    last_profile_read: Instant,
    last_scan_attempt: Instant,
    /// When the score list was last walked. See the read site in `poll`.
    last_leaderboard_read: Instant,
    pub cached_packet: crate::v2::TosuV2Packet,
    pub enable_pp: bool,
    pub enable_hit_errors: bool,
    cached_hit_errors_total_hits: u32,
    cached_hit_errors: Arc<[i16]>,
    cached_unstable_rate: f64,
    /// tosu's slider-break inference (`states/gameplay.ts:242-249`). `prev_combo`,
    /// `prev_miss` and `prev_max_combo` are the previous poll's values, not memory reads.
    prev_combo: i32,
    prev_miss: i32,
    prev_max_combo: i32,
    slider_breaks: i32,
    /// tosu's `gameplay.isDefaultState` latch: set once a play or a results
    /// screen has actually been read, so the exit clear runs once and never
    /// fires for a client that was never in a map.
    play_state_dirty: bool,
    /// The song position (`beatmap.time.live`) of the previous tick, for tosu's
    /// `game.paused`. Not `session.playTime` -- see the note at the call site.
    ///
    /// `None` until the clock has been read twice: there is no previous sample
    /// to compare against before then, and claiming "paused" on the strength of
    /// one reading would report a pause that was never observed.
    previous_play_time: Option<i32>,
    /// Whether an osu! process is currently attached.
    ///
    /// This is the replacement for the invented `client: "none"` /
    /// `state.name: "notRunning"` pair, which had two problems: neither string
    /// is a value tosu can produce (`client` is `ClientType[game.client]`, so
    /// `"none"` only occurs for an older enum, and `state.name` is always
    /// `GameStates[number]`, which has no `notRunning` member), and encoding
    /// "no game" inside the payload forced every consumer to special-case a
    /// sentinel instead of a transport-level failure.
    ///
    /// tosu's actual behaviour is the transport: with no instance, every `/json*`
    /// route throws and answers `500 {"error":"osu is not ready/running"}`
    /// (`packages/server/utils/http.ts:186-207`), and every socket's loop skips
    /// its send entirely (`utils/socket.ts`, `if (!osuInstance || clients.size
    /// === 0) { sleep; continue; }`). The flag travels out of the reader on
    /// `PublishedPacket::attached` so the server can reproduce that.
    attached: bool,
    /// `(beatmap pointer, when the next file attempt is allowed, the current
    /// backoff)`.
    ///
    /// Set when a `.osu` file could not be read or parsed, so that the poll loop
    /// does not re-read and re-parse the same missing file on every tick. See
    /// [`SoloSession::file_attempt_due`].
    file_load_failed: Option<(u64, Instant, Duration)>,
}

/// The first retry interval for a `.osu` file that would not load. Short enough
/// that a map appearing mid-download is picked up promptly.
const FILE_RETRY_INITIAL: Duration = Duration::from_millis(500);

/// The ceiling on that backoff. A map that is genuinely absent then costs one
/// attempt every five seconds for the rest of the match, rather than sixty a
/// second.
const FILE_RETRY_MAX: Duration = Duration::from_secs(5);

/// Whether the `.osu` file behind `beatmap_addr` may be read on this tick.
///
/// A map that is not on disk -- not downloaded, or held by another process --
/// fails the file pass, and the `caches_may_advance` guard then refuses to latch
/// it so the next tick tries again. That is the right behaviour and it is what
/// the 30-tick-freeze bug was about, but it meant the *disk* work ran on every
/// tick too: `populate_beatmap_file_metadata` does a `read_to_string` plus a
/// line-by-line parse, and the pp block does `fs::read` plus
/// `Beatmap::from_bytes` on the same path. At the 60 Hz poll rate that is a full
/// read and two parses of a missing file, sixty times a second, for as long as
/// the map is selected -- and a tournament client selecting a map it has not
/// downloaded is an ordinary situation, not an edge case.
///
/// So the retry stays **unbounded in time and bounded in rate**: a different map
/// is always tried at once, and the same map is re-tried on a backoff that starts
/// at [`FILE_RETRY_INITIAL`] and doubles to [`FILE_RETRY_MAX`]. A map that
/// appears mid-match is picked up within the interval; one that never appears
/// costs a handful of attempts over a map instead of thousands.
///
/// Takes the field by reference rather than `&self`: the poll body already holds
/// an immutable borrow of `self.memory` for the whole function, so a `&self`
/// method would not borrow-check there.
fn file_attempt_due(state: &Option<(u64, Instant, Duration)>, beatmap_addr: u64) -> bool {
    match state {
        Some((failed_addr, retry_at, _)) if *failed_addr == beatmap_addr => {
            Instant::now() >= *retry_at
        }
        // A different beatmap, or no recorded failure at all.
        _ => true,
    }
}

/// Record a failed file attempt for `beatmap_addr` and schedule the next one.
///
/// The delay doubles per consecutive failure on the same map and resets when a
/// different map fails, so selecting a fresh map is never delayed by an earlier
/// map's backoff.
fn note_file_load_failure(state: &mut Option<(u64, Instant, Duration)>, beatmap_addr: u64) {
    let delay = match state {
        Some((failed_addr, _, previous_delay)) if *failed_addr == beatmap_addr => {
            previous_delay.saturating_mul(2).min(FILE_RETRY_MAX)
        }
        _ => FILE_RETRY_INITIAL,
    };
    *state = Some((beatmap_addr, Instant::now() + delay, delay));
}

/// tosu's slider-break inference (`states/gameplay.ts:242-249`). A combo drop
/// that is not accompanied by a miss increment is inferred as a slider break.
/// Preceded by a guard (`gameplay.ts:239-241`) catching retried attempts where
/// max combo drops.
pub(crate) fn infer_slider_breaks(
    prev_combo: &mut i32,
    prev_miss: &mut i32,
    prev_max_combo: &mut i32,
    slider_breaks: &mut i32,
    combo: i16,
    max_combo: i16,
    miss: i16,
) -> i32 {
    let current_combo = combo as i32;
    let current_max_combo = max_combo as i32;
    let current_miss = miss as i32;

    if current_max_combo < *prev_max_combo {
        *prev_combo = 0;
        *slider_breaks = 0;
    }
    if *prev_combo > current_max_combo {
        *prev_combo = 0;
    }
    if current_combo < *prev_combo && current_miss == *prev_miss {
        *slider_breaks += 1;
    }
    *prev_combo = current_combo;
    *prev_miss = current_miss;
    *prev_max_combo = current_max_combo;
    *slider_breaks
}

impl SoloSession {
    pub fn new(
        profile_name: &str,
        pointer_width: Option<usize>,
        scan_limit_bytes: usize,
    ) -> Result<Self> {
        let profile = load_profile(profile_name)?;
        let width = pointer_width.unwrap_or(if profile.pointer_width > 0 {
            profile.pointer_width
        } else {
            4
        });
        Ok(Self {
            profile,
            pointer_width: width,
            scan_limit_bytes,
            pid: None,
            memory: None,
            base_pattern_addr: None,
            status_pattern_addr: None,
            play_time_pattern_addr: None,
            audio_length_pattern_addr: None,
            game_time_pattern_addr: None,
            skin_pattern_addr: None,
            menu_mods_pattern_addr: None,
            user_profile_pattern_addr: None,
            raw_login_status_pattern_addr: None,
            ruleset_container_addr: None,
            game_folder: String::new(),
            songs_folder: String::new(),
            current_checksum: String::new(),
            #[cfg(feature = "pp")]
            cached_beatmap: None,
            #[cfg(feature = "pp")]
            cached_mods: u32::MAX,
            #[cfg(feature = "pp")]
            cached_difficulty_attrs: None,
            #[cfg(feature = "pp")]
            cached_accuracy: crate::v2::PerformanceAccuracy::default(),
            #[cfg(feature = "pp")]
            cached_graph: crate::v2::PrecomputedGraph::default(),
            #[cfg(feature = "pp")]
            cached_gameplay_hits: (0, 0, 0, 0, 0, 0),
            #[cfg(feature = "pp")]
            cached_live_pp: None,
            #[cfg(feature = "pp")]
            cached_results_hits: (0, 0, 0, 0, 0, 0),
            #[cfg(feature = "pp")]
            cached_results_pp: None,
            #[cfg(feature = "pp")]
            cached_idle_pp_key: None,
            #[cfg(feature = "pp")]
            gradual_cursor: crate::pp::calculator::GradualCursor::new(),
            cached_beatmap_metadata: crate::beatmap::BeatmapSnapshot::default(),
            #[cfg(feature = "pp")]
            cached_stats: crate::beatmap::BeatmapStats::default(),
            cached_beatmap_ptr: 0,
            cached_beatmap_id: 0,
            last_skin_read: Instant::now() - Duration::from_secs(10),
            last_profile_read: Instant::now() - Duration::from_secs(10),
            last_scan_attempt: Instant::now() - Duration::from_secs(10),
            // Backdated so the first tick in a play reads the list immediately
            // rather than waiting out the interval on an empty leaderboard.
            last_leaderboard_read: Instant::now() - Duration::from_secs(10),
            cached_packet: crate::v2::TosuV2Packet {
                profile: guest_profile_state(),
                ..Default::default()
            },
            enable_pp: true,
            enable_hit_errors: true,
            cached_hit_errors_total_hits: 0,
            cached_hit_errors: Arc::default(),
            cached_unstable_rate: 0.0,
            prev_combo: 0,
            prev_miss: 0,
            prev_max_combo: 0,
            slider_breaks: 0,
            play_state_dirty: false,
            previous_play_time: None,
            attached: false,
            file_load_failed: None,
        })
    }

    /// Report that there is nothing to read, so a poll without a live process
    /// does not keep serving the last known state.
    ///
    /// The play-derived fields are dropped for the same reason -- a poll that
    /// cannot read must not leave a finished attempt's score and hit counts
    /// looking current -- and the `attached` flag goes false, which is what
    /// actually makes the `/json*` routes answer `500` the way tosu's do.
    ///
    /// What this deliberately does **not** do is write a marker into the
    /// payload. See [`SoloSession::attached`].
    fn mark_not_running(&mut self) {
        self.attached = false;
        clear_play_state_for_new_map(&mut self.cached_packet);
        self.slider_breaks = 0;
        self.prev_combo = 0;
        self.prev_miss = 0;
        self.prev_max_combo = 0;
    }

    /// Whether an osu! process is currently attached. Drives the not-running
    /// contract on every `/json*` route and the sockets' silence.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    pub fn poll(&mut self) -> Result<crate::v2::TosuV2Packet> {
        crate::instr_scope!(SoloPoll);
        // 1. Ensure we have a valid open process
        let mut need_proc_open = true;
        if let Some(mem) = &self.memory {
            if mem.is_alive() {
                need_proc_open = false;
            } else {
                self.pid = None;
                self.memory = None;
                self.base_pattern_addr = None;
                self.status_pattern_addr = None;
                self.play_time_pattern_addr = None;
                self.audio_length_pattern_addr = None;
                self.game_time_pattern_addr = None;
                self.skin_pattern_addr = None;
                self.menu_mods_pattern_addr = None;
                self.user_profile_pattern_addr = None;
                self.raw_login_status_pattern_addr = None;
                self.ruleset_container_addr = None;
                self.current_checksum.clear();
                #[cfg(feature = "pp")]
                {
                    self.cached_beatmap = None;
                    self.cached_difficulty_attrs = None;
                    self.cached_live_pp = None;
                    self.cached_results_pp = None;
                    self.cached_idle_pp_key = None;
                    self.cached_mods = u32::MAX;
                }
                self.mark_not_running();
                return Ok(self.cached_packet.clone());
            }
        }

        if need_proc_open {
            let procs = list_processes(Some("osu!.exe")).unwrap_or_default();
            if procs.is_empty() {
                self.pid = None;
                self.memory = None;
                self.mark_not_running();
                return Ok(self.cached_packet.clone());
            }

            let pid = procs[0].pid;
            let mem = ProcessMemory::open_with_pointer_size(pid, Some(self.pointer_width))?;
            if let Ok(exe_path) = mem.process_image_path() {
                let p = std::path::Path::new(&exe_path);
                if let Some(parent) = p.parent() {
                    self.game_folder = parent.to_string_lossy().to_string();
                    self.songs_folder = parent.join("Songs").to_string_lossy().to_string();
                }
            }
            self.pid = Some(pid);
            self.memory = Some(mem);
            self.base_pattern_addr = None;
            self.status_pattern_addr = None;
            self.play_time_pattern_addr = None;
            self.audio_length_pattern_addr = None;
            self.game_time_pattern_addr = None;
            self.skin_pattern_addr = None;
            self.menu_mods_pattern_addr = None;
            self.user_profile_pattern_addr = None;
            self.raw_login_status_pattern_addr = None;
            self.ruleset_container_addr = None;
            self.current_checksum.clear();
            #[cfg(feature = "pp")]
            {
                self.cached_beatmap = None;
                self.cached_difficulty_attrs = None;
                self.cached_live_pp = None;
                self.cached_results_pp = None;
                self.cached_mods = u32::MAX;
            }
        }

        // Every path above either returns or leaves `memory` populated, so this
        // cannot be `None`. It is a let-else rather than an `unwrap` anyway: the
        // release profile aborts on panic, and reporting "not attached" is a far
        // better failure than the process vanishing mid-match.
        let Some(memory) = self.memory.as_ref() else {
            self.mark_not_running();
            return Ok(self.cached_packet.clone());
        };
        // Past this point every return path has a live handle, so this is the one
        // place that has to clear the flag `mark_not_running` sets.
        self.attached = true;
        let can_scan = self.last_scan_attempt.elapsed() >= std::time::Duration::from_millis(1000);
        let mut attempted_scan = false;

        // 2. Scan status_ptr if not cached
        if self.status_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("status_ptr") {
                if let Ok(pat) = BytePattern::parse(pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&pat, None, 1, self.scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, pat_off) {
                                self.status_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }

        // 3. Scan base_addr if not cached
        if self.base_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, _)) = self.profile.pattern("base_addr") {
                if let Ok(pat) = BytePattern::parse(pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&pat, None, 1, self.scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            self.base_pattern_addr = Some(first);
                        }
                    }
                }
            }
        }

        // 4. Scan play_time_addr if not cached
        if self.play_time_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("play_time_addr") {
                if let Ok(pat) = BytePattern::parse(pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&pat, None, 1, self.scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, pat_off) {
                                self.play_time_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }

        // 5. Scan audio length pointer if not cached
        if self.audio_length_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("get_audio_length_ptr") {
                self.audio_length_pattern_addr =
                    find_pattern(memory, pat_src, pat_off, self.scan_limit_bytes).ok();
            }
        }

        // 6. Scan game time pointer if not cached
        if self.game_time_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("game_time_ptr") {
                self.game_time_pattern_addr =
                    find_pattern(memory, pat_src, pat_off, self.scan_limit_bytes).ok();
            }
        }

        // 7. Scan skin pointer if not cached
        if self.skin_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("skin_data_addr") {
                self.skin_pattern_addr =
                    find_pattern(memory, pat_src, pat_off, self.scan_limit_bytes).ok();
            }
        }

        // 8. Scan menu_mods_ptr if not cached
        if self.menu_mods_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("menu_mods_ptr") {
                if let Ok(pat) = BytePattern::parse(pat_src) {
                    if let Ok(matches) = memory.scan_pattern(&pat, None, 1, self.scan_limit_bytes) {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, pat_off) {
                                self.menu_mods_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }

        if self.user_profile_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("user_profile_ptr") {
                self.user_profile_pattern_addr =
                    find_pattern(memory, pat_src, pat_off, self.scan_limit_bytes).ok();
            }
        }
        if self.raw_login_status_pattern_addr.is_none() && can_scan {
            attempted_scan = true;
            if let Ok((pat_src, pat_off)) = self.profile.pattern("raw_login_status_ptr") {
                self.raw_login_status_pattern_addr =
                    find_pattern(memory, pat_src, pat_off, self.scan_limit_bytes).ok();
            }
        }

        if attempted_scan {
            self.last_scan_attempt = Instant::now();
        }

        self.cached_packet.client = "stable".to_string();
        self.cached_packet.server = "ppy.sh".to_string();

        // 1. Read state first to detect state transitions
        let mut current_state_num = 0;
        let mut state_changed = false;
        if let Some(status_addr) = self.status_pattern_addr
            && let Ok(state) = memory.read_indirect_pointer(status_addr)
        {
            let state = state as i32;
            current_state_num = state;
            if self.cached_packet.state.number != state {
                state_changed = true;
                self.cached_packet.state.number = state;
                self.cached_packet.state.name = crate::v2::osu_state_name(state).to_string();
            }
            self.cached_packet.game.focused = memory.is_foreground();
        } else {
            self.cached_packet.game.focused = memory.is_foreground();
        }

        // Leaving the map for song select, or for any other state the play
        // block is not read in, must drop the finished run's numbers instead
        // of keeping them until a different beatmap happens to be checksummed.
        if state_changed && should_clear_play_state(self.play_state_dirty, current_state_num) {
            clear_play_state_for_new_map(&mut self.cached_packet);
            self.play_state_dirty = false;
            self.cached_hit_errors = Arc::default();
            self.cached_hit_errors_total_hits = 0;
            self.cached_unstable_rate = 0.0;
            self.slider_breaks = 0;
            self.prev_combo = 0;
            self.prev_miss = 0;
            self.prev_max_combo = 0;
            // The cached list went with the rest of the play state, so the next
            // play must not have to wait out the leaderboard interval to refill it.
            // (`Instant::now()` rather than the loop's `now`, which is bound later.)
            self.last_leaderboard_read = Instant::now() - LEADERBOARD_INTERVAL;
            #[cfg(feature = "pp")]
            {
                self.cached_gameplay_hits = (0, 0, 0, 0, 0, 0);
                self.cached_live_pp = None;
                self.cached_results_hits = (0, 0, 0, 0, 0, 0);
                self.cached_results_pp = None;
                // The idle PP path only recomputes when its (beatmap, mods) key
                // changes, and the clear just zeroed `play.pp`. Dropping the key
                // is what lets it refill, which is the shape tosu's
                // `beatmapPP.resetAttributes()` leaves behind: a zeroed current
                // against a real fc for the highlighted map.
                self.cached_idle_pp_key = None;
            }
        }

        if state_changed && current_state_num == 2 {
            self.slider_breaks = 0;
            self.prev_combo = 0;
            self.prev_miss = 0;
            self.prev_max_combo = 0;
        }

        // 2. Throttled Skin & Profile reads (on state change or low-frequency heartbeat)
        let now = Instant::now();
        if let Some(skin_addr) = self.skin_pattern_addr {
            if state_changed || now.duration_since(self.last_skin_read) >= Duration::from_secs(3) {
                self.last_skin_read = now;
                crate::instr_scope!(SoloSkin);
                let skin = read_skin_folder(memory, skin_addr).unwrap_or_default();
                self.cached_packet.folders.skin = skin.clone();
                self.cached_packet.direct_path.skin_folder = skin;
            }
        }
        if let (Some(user_pattern), Some(login_pattern)) = (
            self.user_profile_pattern_addr,
            self.raw_login_status_pattern_addr,
        ) {
            if state_changed || now.duration_since(self.last_profile_read) >= Duration::from_secs(5)
            {
                self.last_profile_read = now;
                if let Ok(profile) = read_local_profile(memory, user_pattern, login_pattern) {
                    self.cached_packet.profile = profile_state_from_local(&profile);
                }
            }
        }

        if let Some(game_time_addr) = self.game_time_pattern_addr
            && let Ok(game_time) = memory.read_indirect_pointer(game_time_addr)
        {
            self.cached_packet.session.play_time = game_time as i32;
        }

        // Read menu mods if in song select or menu
        if current_state_num != 2 && current_state_num != 7 {
            self.cached_packet.beatmap.stats.stars.live = 0.0;
            self.cached_hit_errors_total_hits = 0;
            self.cached_hit_errors = Arc::default();
            self.cached_unstable_rate = 0.0;
            if let Some(mods_addr) = self.menu_mods_pattern_addr
                && let Ok(mods_val) = memory.read_indirect_pointer(mods_addr)
            {
                let mods_val = mods_val as u32;
                self.cached_packet.play.mods =
                    crate::v2::create_mods_state(mods_val, &crate::client::format_mods(mods_val));
            }
        }

        // 3. Live audio playback time (updates continuously at 60 Hz)
        let live_time = crate::beatmap::read_live_time(memory, self.play_time_pattern_addr);
        self.cached_packet.beatmap.time.live = live_time;

        // `game.paused` is not a flag anywhere in osu!. tosu derives it from two
        // consecutive samples of the song clock
        // (`tosu-sourcecode/packages/tosu/src/states/global.ts:68-69`):
        //
        //     this.paused = this.previousPlayTime === this.playTime;
        //     this.previousPlayTime = this.playTime;
        //
        // **The clock is the song position, not `session.playTime`.** Those are
        // two different reads in tosu: `session.playTime` is `global.gameTime`
        // (`buildResultV2.ts:147`) and `beatmap.time.live` is `global.playTime`
        // (`:333`), and it is the *precise* one that `paused` compares.
        // `gameTime` is a session-wide counter that keeps advancing while the song
        // is frozen -- measured live: `gameTime` ran 688170 -> 692970 across six
        // samples while the song position sat still at 11575, and tosu reported
        // `paused: true` throughout. Comparing `session.playTime` therefore reads
        // "not paused" for a paused map, and also disagrees with itself, because
        // that counter is coarse enough to repeat between two 16 ms polls.
        //
        // The first tick has no previous sample to compare against. tosu starts
        // both at 0, so its first tick reports `paused: true`; seeding with
        // `None` reports `false` instead, which is the honest answer for a clock
        // that has only been read once.
        self.cached_packet.game.paused = is_paused(self.previous_play_time, live_time);
        self.previous_play_time = Some(live_time);

        // 4. Hierarchical beatmap reading (pointer-gated)
        if let Some(base_addr) = self.base_pattern_addr {
            if let Ok(beatmap_addr) = crate::beatmap::read_beatmap_ptr(memory, base_addr) {
                if beatmap_addr == 0 {
                    // osu! clears the beatmap pointer the moment a map ends, so
                    // wiping the snapshot here would blank an overlay's header
                    // between maps. The last map is kept until a different one
                    // is actually selected; it goes away with the rest of the
                    // packet when osu! itself does.
                    self.cached_beatmap_ptr = 0;
                    self.cached_beatmap_id = 0;
                } else {
                    let beatmap_ptr_changed = beatmap_addr != self.cached_beatmap_ptr;
                    let live_id = crate::beatmap::read_beatmap_id(memory, beatmap_addr);
                    let beatmap_changed = crate::beatmap::beatmap_refresh_needed(
                        beatmap_addr,
                        self.cached_beatmap_ptr,
                        live_id,
                        self.cached_beatmap_id,
                    );
                    #[cfg(feature = "pp")]
                    let active_mods = self.cached_packet.play.mods.number;
                    #[cfg(feature = "pp")]
                    let mods_changed =
                        self.cached_mods != active_mods || self.cached_difficulty_attrs.is_none();

                    if beatmap_changed {
                        if beatmap_ptr_changed {
                            self.cached_beatmap_ptr = beatmap_addr;
                        }
                        if let Ok(mut bm) = crate::beatmap::read_beatmap_from_ptr(
                            memory,
                            beatmap_addr,
                            base_addr,
                            live_time,
                            self.pointer_width,
                        ) {
                            if bm.id > 0 || !bm.title.is_empty() {
                                if let Some(audio_addr) = self.audio_length_pattern_addr
                                    && let Ok(audio_ptr) = memory.read_indirect_pointer(audio_addr)
                                    && let Ok(audio_length) =
                                        memory.read_f64(audio_ptr.saturating_add(4))
                                {
                                    bm.time.mp3_length = audio_length as i32;
                                }

                                // osu! fills `BeatmapInfo` in field by field, so
                                // the title and the id can already be current while
                                // the md5 at +0x6C is still the previous map's, or
                                // empty. tosu treats such a read as 'not-ready' and
                                // skips the whole state update
                                // (`states/beatmap.ts:355-377`,
                                // `instances/osuInstance.ts:113`), so the previously
                                // published beatmap stands and the next tick re-reads.
                                // Requiring a checksum is also what makes Restore
                                // unreachable for a different map, which is the whole
                                // of the stale-metadata bug: that branch used to fire
                                // whenever the held checksum was merely non-empty, and
                                // stamped the previous map's source, tags, object
                                // counts, first/last object, bpm and mp3 length onto
                                // the new one.
                                let read = beatmap_read_action(
                                    &bm.title,
                                    &bm.checksum,
                                    &self.current_checksum,
                                    live_id,
                                    self.cached_beatmap_id,
                                );
                                let resolved = read != BeatmapReadAction::Retry;
                                if read == BeatmapReadAction::Retry {
                                    // Leave the id uncached so the next tick tries
                                    // again. Deliberately unbounded: the cost is one
                                    // read per tick, and the 30-tick bound that used
                                    // to stop that only saved the read at the price
                                    // of permanently freezing whatever partial read
                                    // happened to be in hand. tosu retries forever
                                    // too, and so does TournamentSession.
                                    self.cached_beatmap_id = 0;
                                }

                                if read == BeatmapReadAction::Load {
                                    // `self.memory` is borrowed for this whole
                                    // function, so write the disjoint fields in
                                    // place rather than through a `&mut self` method.
                                    clear_play_state_for_new_map(&mut self.cached_packet);
                                    self.cached_hit_errors = Arc::default();
                                    self.cached_hit_errors_total_hits = 0;
                                    self.cached_unstable_rate = 0.0;
                                    self.slider_breaks = 0;
                                    self.prev_combo = 0;
                                    self.prev_miss = 0;
                                    self.prev_max_combo = 0;
                                    // As above: a freshly loaded map starts with an
                                    // empty leaderboard, so read it on the first tick.
                                    self.last_leaderboard_read =
                                        Instant::now() - LEADERBOARD_INTERVAL;
                                    #[cfg(feature = "pp")]
                                    {
                                        self.cached_gameplay_hits = (0, 0, 0, 0, 0, 0);
                                        self.cached_results_hits = (0, 0, 0, 0, 0, 0);
                                        self.cached_live_pp = None;
                                        self.cached_results_pp = None;
                                        self.cached_idle_pp_key = None;
                                    }
                                    let osu_path = std::path::Path::new(&self.songs_folder)
                                        .join(&bm.folder)
                                        .join(&bm.filename);
                                    // Whether the two disk operations below may run
                                    // on this tick. A map that is not on disk -- not
                                    // downloaded, or locked -- fails both, and the
                                    // `else` arm below then forces a full re-read on
                                    // the *next* tick too, so the pair used to run 60
                                    // times a second for as long as the map was
                                    // selected. Memory reads are cheap; a
                                    // `read_to_string` plus a line-by-line parse
                                    // plus `fs::read` plus `Beatmap::from_bytes` on
                                    // the same file is not. Bounded by a backoff
                                    // while still retrying forever, so a map that
                                    // appears mid-match is picked up within the
                                    // interval rather than never.
                                    let file_attempt_due =
                                        file_attempt_due(&self.file_load_failed, beatmap_addr);
                                    // The game's current ruleset, which
                                    // `read_beatmap_from_ptr` has just put in
                                    // `mode.number`. Captured before the file
                                    // pass overwrites it with the map's own.
                                    let current_ruleset = bm.current_ruleset;
                                    let metadata_ok = if file_attempt_due {
                                        let ok = crate::beatmap::populate_beatmap_file_metadata(
                                            &mut bm, &osu_path,
                                        );
                                        if ok {
                                            crate::beatmap::apply_beatmap_ruleset(
                                                &mut bm,
                                                current_ruleset,
                                            );
                                        }
                                        ok
                                    } else {
                                        // Skipped, not attempted: report it as
                                        // unloadable so the caches keep their
                                        // "do not latch this" behaviour.
                                        false
                                    };
                                    self.cached_beatmap_metadata = bm.clone();

                                    #[cfg(feature = "pp")]
                                    let file_ok = if file_attempt_due {
                                        crate::instr_scope!(BeatmapFileRead);
                                        match std::fs::read(&osu_path) {
                                            Ok(bytes) => {
                                                crate::instr_scope!(BeatmapParse);
                                                match rosu_pp::Beatmap::from_bytes(&bytes) {
                                                    Ok(map) => {
                                                        self.cached_beatmap = Some(map);
                                                        true
                                                    }
                                                    Err(_) => {
                                                        self.cached_beatmap = None;
                                                        false
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                self.cached_beatmap = None;
                                                false
                                            }
                                        }
                                    } else {
                                        self.cached_beatmap = None;
                                        false
                                    };
                                    #[cfg(not(feature = "pp"))]
                                    let file_ok = true;

                                    if metadata_ok && file_ok {
                                        self.file_load_failed = None;
                                    } else {
                                        note_file_load_failure(
                                            &mut self.file_load_failed,
                                            beatmap_addr,
                                        );
                                    }

                                    #[cfg(feature = "pp")]
                                    {
                                        self.cached_difficulty_attrs = None;
                                        self.cached_mods = u32::MAX;
                                    }

                                    if caches_may_advance(read, metadata_ok, file_ok) {
                                        self.current_checksum = bm.checksum.clone();
                                        self.cached_beatmap_id = live_id;
                                    } else {
                                        // Force a full re-read next tick rather than
                                        // latching a map we could not load.
                                        self.cached_beatmap_id = 0;
                                        self.cached_beatmap_ptr = 0;
                                    }
                                } else if read == BeatmapReadAction::Restore {
                                    // Restore cached file metadata without reading disk!
                                    bm.source = self.cached_beatmap_metadata.source.clone();
                                    bm.tags = self.cached_beatmap_metadata.tags.clone();
                                    bm.stats.objects =
                                        self.cached_beatmap_metadata.stats.objects.clone();
                                    bm.time.first_object =
                                        self.cached_beatmap_metadata.time.first_object;
                                    bm.time.last_object =
                                        self.cached_beatmap_metadata.time.last_object;
                                    if bm.time.mp3_length == 0 {
                                        bm.time.mp3_length =
                                            self.cached_beatmap_metadata.time.mp3_length;
                                    }
                                    bm.stats.bpm = self.cached_beatmap_metadata.stats.bpm.clone();
                                    restore_beatmap_ruleset(&mut bm, &self.cached_beatmap_metadata);
                                }

                                // An unresolved read publishes nothing, which is what
                                // tosu's `continue` amounts to: the last good beatmap
                                // stays in the packet instead of being overwritten by a
                                // partial one whose stars still come from the previous
                                // map's parsed file.
                                if resolved {
                                    #[cfg(feature = "pp")]
                                    if let Some(map) = &self.cached_beatmap {
                                        self.cached_mods = active_mods;
                                        let mods_legacy =
                                            crate::pp::calculator::parse_mods_bits(active_mods);
                                        crate::instr_scope!(PpDifficulty);
                                        let diff = rosu_pp::Difficulty::new()
                                            .mods(mods_legacy)
                                            .calculate(map);
                                        crate::beatmap::populate_beatmap_statistics_with_diff(
                                            &mut bm,
                                            map,
                                            &diff,
                                            active_mods,
                                        );
                                        self.cached_stats = bm.stats.clone();
                                        self.cached_beatmap_metadata.time.last_object =
                                            bm.time.last_object;
                                        self.cached_accuracy =
                                            crate::pp::calculator::calc_accuracy_table_from_diff(
                                                &diff,
                                            );
                                        crate::instr_scope!(GraphBuild);
                                        self.cached_graph = performance_graph(
                                            map,
                                            active_mods,
                                            bm.time.first_object,
                                            bm.time.last_object,
                                            bm.time.mp3_length,
                                        );
                                        self.cached_difficulty_attrs = Some(diff);
                                        self.cached_packet.performance.accuracy =
                                            self.cached_accuracy.clone();
                                        self.cached_packet.performance.graph =
                                            self.cached_graph.clone();
                                        // Same coupling as the mods path: the
                                        // builder above zeroed `stars.live`, and
                                        // the live pair has to be rebuilt. On a
                                        // fresh map the judged count is usually 0
                                        // and therefore already equal to the
                                        // cached tuple, so nothing downstream
                                        // would notice on its own.
                                        self.cached_live_pp = None;
                                    }

                                    self.cached_packet.folders.game = self.game_folder.clone();
                                    self.cached_packet.folders.songs = self.songs_folder.clone();
                                    self.cached_packet.folders.beatmap = bm.folder.clone();
                                    self.cached_packet.files.beatmap = bm.filename.clone();
                                    self.cached_packet.files.background =
                                        bm.background_filename.clone();
                                    self.cached_packet.files.audio = bm.audio_filename.clone();
                                    self.cached_packet.direct_path.beatmap_folder =
                                        bm.folder.clone();
                                    self.cached_packet.direct_path.beatmap_file =
                                        join_beatmap_path(&bm.folder, &bm.filename);
                                    self.cached_packet.direct_path.beatmap_background =
                                        join_beatmap_path(&bm.folder, &bm.background_filename);
                                    self.cached_packet.direct_path.beatmap_audio =
                                        join_beatmap_path(&bm.folder, &bm.audio_filename);

                                    self.cached_packet.beatmap = bm;
                                }
                            }
                        }
                    } else {
                        // Same beatmap pointer
                        #[cfg(feature = "pp")]
                        if mods_changed {
                            if let Some(map) = &self.cached_beatmap {
                                self.cached_mods = active_mods;
                                let mods_legacy =
                                    crate::pp::calculator::parse_mods_bits(active_mods);
                                crate::instr_scope!(PpDifficulty);
                                let diff =
                                    rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
                                crate::beatmap::populate_beatmap_statistics_with_diff(
                                    &mut self.cached_packet.beatmap,
                                    map,
                                    &diff,
                                    active_mods,
                                );
                                self.cached_stats = self.cached_packet.beatmap.stats.clone();
                                self.cached_accuracy =
                                    crate::pp::calculator::calc_accuracy_table_from_diff(&diff);
                                crate::instr_scope!(GraphBuild);
                                self.cached_graph = performance_graph(
                                    map,
                                    active_mods,
                                    self.cached_packet.beatmap.time.first_object,
                                    self.cached_packet.beatmap.time.last_object,
                                    self.cached_packet.beatmap.time.mp3_length,
                                );
                                self.cached_difficulty_attrs = Some(diff);
                                self.cached_packet.performance.accuracy =
                                    self.cached_accuracy.clone();
                                self.cached_packet.performance.graph = self.cached_graph.clone();
                            }

                            // `populate_beatmap_statistics_with_diff` resets
                            // `stars.live` to its "no live play" default of 0, and
                            // `stars.live` and `play.pp` are two outputs of the
                            // same cursor step below. The live guard only reopens
                            // when the judged counts or `g.mods` change, and this
                            // path is keyed on the *menu* mods, so a play that is
                            // paused -- or simply not judging anything this tick --
                            // would keep a zeroed live rating and a stale pp for as
                            // long as the counts stayed put. Invalidate next to
                            // the write that forces it.
                            self.cached_live_pp = None;
                        }

                        if self.cached_packet.beatmap.time.mp3_length == 0 {
                            if let Some(audio_addr) = self.audio_length_pattern_addr
                                && let Ok(audio_ptr) = memory.read_indirect_pointer(audio_addr)
                                && let Ok(audio_length) =
                                    memory.read_f64(audio_ptr.saturating_add(4))
                            {
                                self.cached_packet.beatmap.time.mp3_length = audio_length as i32;
                            }
                        }
                    }
                }
            }
        }

        #[cfg(feature = "pp")]
        if let Some(map) = self.cached_beatmap.as_ref() {
            let live = self.cached_packet.beatmap.time.live as f64;
            self.cached_packet.beatmap.is_kiai = map
                .effect_points
                .iter()
                .rev()
                .find(|ep| ep.time <= live)
                .map_or(false, |ep| ep.kiai);
            self.cached_packet.beatmap.is_break = map
                .breaks
                .iter()
                .any(|b| live >= b.start_time && live <= b.end_time);
        }

        // 8. Read gameplay or resultsScreen based on state
        if self.ruleset_container_addr.is_none() && can_scan {
            self.ruleset_container_addr = crate::client::resolve_ruleset_container(
                memory,
                &self.profile,
                self.scan_limit_bytes,
            )
            .ok();
        }

        let active_ruleset_addr = self
            .ruleset_container_addr
            .and_then(|addr| crate::client::read_active_ruleset(memory, addr));

        if current_state_num == 2 {
            if let Some(ruleset_addr) = active_ruleset_addr {
                let cached = Some((
                    self.cached_hit_errors_total_hits,
                    &self.cached_hit_errors,
                    self.cached_unstable_rate,
                ));
                if let Ok(mut g) = crate::client::read_gameplay_state_cached(
                    memory,
                    ruleset_addr,
                    cached,
                    self.cached_packet.beatmap.stats.objects.total,
                ) {
                    let total_hits = (g.hit_300 + g.hit_100 + g.hit_50 + g.hit_miss) as u32;
                    self.cached_hit_errors_total_hits = total_hits;
                    self.cached_unstable_rate = g.unstable_rate;
                    if self.enable_hit_errors {
                        self.cached_hit_errors = Arc::clone(&g.hit_error_array);
                    } else {
                        self.cached_hit_errors = Arc::default();
                        g.hit_error_array = Arc::default();
                    }
                    self.cached_packet.play.failed = g.player_hp <= 0.0;
                    self.cached_packet.play.player_name = g.player_name;
                    self.cached_packet.play.mode = crate::v2::OsuStatusState {
                        number: g.mode,
                        name: ruleset_name(g.mode).to_string(),
                    };
                    self.cached_packet.play.score = g.score;
                    self.cached_packet.play.accuracy = g.accuracy;
                    self.cached_packet.play.combo.current = g.combo as i32;
                    self.cached_packet.play.combo.max = g.max_combo as i32;
                    self.cached_packet.play.hits.n300 = g.hit_300 as i32;
                    self.cached_packet.play.hits.n100 = g.hit_100 as i32;
                    self.cached_packet.play.hits.n50 = g.hit_50 as i32;
                    self.cached_packet.play.hits.n0 = g.hit_miss as i32;
                    self.cached_packet.play.hits.geki = g.hit_geki as i32;
                    self.cached_packet.play.hits.katu = g.hit_katu as i32;
                    self.cached_packet.play.hits.slider_breaks = infer_slider_breaks(
                        &mut self.prev_combo,
                        &mut self.prev_miss,
                        &mut self.prev_max_combo,
                        &mut self.slider_breaks,
                        g.combo,
                        g.max_combo,
                        g.hit_miss,
                    );
                    self.cached_packet.play.health_bar.normal = g.player_hp / 2.0;
                    self.cached_packet.play.health_bar.smooth = g.player_hp_smooth / 2.0;
                    self.cached_packet.play.hit_error_array = g.hit_error_array;
                    self.cached_packet.play.unstable_rate = g.unstable_rate;
                    self.cached_packet.play.rank.current = g.grade;
                    self.cached_packet.play.rank.max_this_play = g.grade_max;
                    // osu! only populates the key overlay during a play, and this
                    // is the play-state read, so this is the one place the value
                    // is live. tosu reads it under the same gate
                    // (`osuInstance.ts:225-238`: `updateKeyOverlay()` in
                    // `GameState.play` only, `resetKeyOverlay()` in every other
                    // state), and the two other consumers of the read -- the
                    // precise payload and v1's `gameplay.keyOverlay` -- are
                    // reshapes of this packet rather than separate reads.
                    self.cached_packet.play.key_overlay = g.key_overlay;
                    // The leaderboard is read from the same ruleset base as the
                    // key overlay, and osu! only populates its score list during
                    // a play, so the play-state read is the one place it is live.
                    // tosu reads it under the same gate: `updateLeaderboard()` is
                    // called from `gameplay.ts:253`, inside the play branch.
                    //
                    // Rate-limited to 1 Hz. Walking the list costs a handful of
                    // `ReadProcessMemory` calls per row on top of the three
                    // pointer hops, and at the 60 Hz poll rate that is tens of
                    // thousands of kernel transitions a second for a list whose
                    // contents only move when someone finishes or passes a score.
                    // A new map is not on that clock -- `clear_play_state_for_new_map`
                    // empties the cached list, so the timer is backdated there and
                    // the first tick of a new play reads it immediately.
                    if now.duration_since(self.last_leaderboard_read) >= LEADERBOARD_INTERVAL {
                        self.last_leaderboard_read = now;
                        self.cached_packet.leaderboard =
                            crate::client::read_leaderboard(memory, ruleset_addr, g.mode);
                    }
                    if self.cached_packet.play.mods.number != g.mods {
                        self.cached_packet.play.mods =
                            crate::v2::create_mods_state(g.mods, &g.mods_str);
                    }
                    self.play_state_dirty = true;

                    #[cfg(feature = "pp")]
                    if self.enable_pp {
                        if let Some(map) = &self.cached_beatmap {
                            let current_hits = (
                                g.combo as u32,
                                g.hit_300 as u32,
                                g.hit_100 as u32,
                                g.hit_50 as u32,
                                g.hit_miss as u32,
                                g.mods,
                            );
                            if self.cached_gameplay_hits != current_hits
                                || self.cached_live_pp.is_none()
                            {
                                self.cached_gameplay_hits = current_hits;
                                let mods_legacy = crate::pp::calculator::parse_mods_bits(g.mods);
                                // The cursor only folds the objects judged
                                // *since the last time this ran*, so a tick that
                                // saw no judgement costs nothing and a tick that
                                // saw a few pays for those few. `stars.live` and
                                // `play.pp` come out of the same step, which is
                                // what keeps them agreeing.
                                //
                                // Scoped so the cursor's borrow ends before the
                                // packet is written: the two are separate fields,
                                // but the borrow has to be over by the time
                                // `self.cached_live_pp` is assigned.
                                let (live_stars, live_pp) = {
                                    let live_attrs = self.gradual_cursor.advance_to(
                                        self.cached_packet.beatmap.id as u32,
                                        map,
                                        mods_legacy,
                                        total_hits,
                                    );
                                    let stars = live_attrs
                                        .map(crate::pp::calculator::live_stars)
                                        .unwrap_or(0.0);
                                    let pp = self.cached_difficulty_attrs.as_ref().map(|full| {
                                        crate::pp::calculator::calc_detailed_live_and_fc_pp(
                                            live_attrs,
                                            full,
                                            mods_legacy,
                                            g.combo as u32,
                                            g.hit_300 as u32,
                                            g.hit_100 as u32,
                                            g.hit_50 as u32,
                                            g.hit_miss as u32,
                                        )
                                    });
                                    (stars, pp)
                                };
                                self.cached_packet.beatmap.stats.stars.live = live_stars;
                                if let Some(pp) = live_pp {
                                    self.cached_live_pp = Some(pp);
                                }
                            }
                            if let Some(pp) = &self.cached_live_pp {
                                self.cached_packet.play.pp = pp.clone();
                            }
                        }
                    } else {
                        self.cached_packet.play.pp = crate::pp::LivePpResult::default();
                    }
                }
            }
        } else if current_state_num == 7 {
            // resultScreen
            self.cached_packet.beatmap.stats.stars.live =
                self.cached_packet.beatmap.stats.stars.total;

            if let Some(ruleset_addr) = active_ruleset_addr {
                if let Ok(g) = crate::client::read_gameplay_state(
                    memory,
                    ruleset_addr,
                    self.cached_packet.beatmap.stats.objects.total,
                ) {
                    self.cached_packet.play.player_name = g.player_name;
                    self.cached_packet.play.mode = crate::v2::OsuStatusState {
                        number: g.mode,
                        name: ruleset_name(g.mode).to_string(),
                    };
                    self.cached_packet.play.score = g.score;
                    self.cached_packet.play.accuracy = g.accuracy;
                    self.cached_packet.play.combo.current = g.combo as i32;
                    self.cached_packet.play.combo.max = g.max_combo as i32;
                    self.cached_packet.play.hits.n300 = g.hit_300 as i32;
                    self.cached_packet.play.hits.n100 = g.hit_100 as i32;
                    self.cached_packet.play.hits.n50 = g.hit_50 as i32;
                    self.cached_packet.play.hits.n0 = g.hit_miss as i32;
                    self.cached_packet.play.hits.geki = g.hit_geki as i32;
                    self.cached_packet.play.hits.katu = g.hit_katu as i32;
                    self.cached_packet.play.health_bar.normal = g.player_hp / 2.0;
                    self.cached_packet.play.health_bar.smooth = g.player_hp_smooth / 2.0;
                    self.cached_packet.play.hit_error_array = g.hit_error_array;
                    self.cached_packet.play.unstable_rate = g.unstable_rate;
                    self.cached_packet.play.rank.current = g.grade;
                    self.cached_packet.play.rank.max_this_play = g.grade_max;
                    // The results screen is **not** `GameState.play`, and tosu's
                    // precise loop calls `gameplay.resetKeyOverlay()` in every
                    // state but that one (`osuInstance.ts:234-237`). So the
                    // buttons go back to neutral here even though the rest of the
                    // play block is deliberately left frozen -- a results screen
                    // showing the last play's score with the last play's keys
                    // still held would be reporting a state that does not exist.
                    self.cached_packet.play.key_overlay = Default::default();
                    if self.cached_packet.play.mods.number != g.mods {
                        self.cached_packet.play.mods =
                            crate::v2::create_mods_state(g.mods, &g.mods_str);
                    }
                    self.play_state_dirty = true;
                }
                if let Ok(res) = crate::client::read_result_screen_state(memory, ruleset_addr) {
                    let mods_state = crate::v2::create_mods_state(res.mods, &res.mods_str);
                    self.cached_packet.results_screen.score_id = res.online_id;
                    self.cached_packet.results_screen.player_name = res.player_name.clone();
                    self.cached_packet.results_screen.name = res.player_name.clone();
                    self.cached_packet.results_screen.mode = crate::v2::OsuStatusState {
                        number: res.mode,
                        name: ruleset_name(res.mode).to_string(),
                    };
                    self.cached_packet.results_screen.score = res.score;
                    self.cached_packet.results_screen.accuracy = res.accuracy;
                    self.cached_packet.results_screen.max_combo = res.max_combo as i32;
                    self.cached_packet.results_screen.rank = res.grade.clone();
                    self.cached_packet.results_screen.hits.n300 = res.hit_300 as i32;
                    self.cached_packet.results_screen.hits.n100 = res.hit_100 as i32;
                    self.cached_packet.results_screen.hits.n50 = res.hit_50 as i32;
                    self.cached_packet.results_screen.hits.n0 = res.hit_miss as i32;
                    self.cached_packet.results_screen.hits.geki = res.hit_geki as i32;
                    self.cached_packet.results_screen.hits.katu = res.hit_katu as i32;
                    self.cached_packet.results_screen.mods = mods_state.clone();
                    self.cached_packet.results_screen.created_at = res.created_at;

                    self.cached_packet.play.player_name = res.player_name.clone();
                    self.cached_packet.play.mode = crate::v2::OsuStatusState {
                        number: res.mode,
                        name: ruleset_name(res.mode).to_string(),
                    };
                    self.cached_packet.play.score = res.score;
                    self.cached_packet.play.accuracy = res.accuracy;
                    self.cached_packet.play.combo.current = res.max_combo as i32;
                    self.cached_packet.play.combo.max = res.max_combo as i32;
                    self.cached_packet.play.hits.n300 = res.hit_300 as i32;
                    self.cached_packet.play.hits.n100 = res.hit_100 as i32;
                    self.cached_packet.play.hits.n50 = res.hit_50 as i32;
                    self.cached_packet.play.hits.n0 = res.hit_miss as i32;
                    self.cached_packet.play.hits.geki = res.hit_geki as i32;
                    self.cached_packet.play.hits.katu = res.hit_katu as i32;
                    self.cached_packet.play.rank.current = res.grade.clone();
                    self.cached_packet.play.rank.max_this_play = res.grade.clone();
                    self.cached_packet.play.mods = mods_state.clone();
                    self.play_state_dirty = true;
                    #[cfg(feature = "pp")]
                    if self.cached_mods != res.mods {
                        if let Some(map) = &self.cached_beatmap {
                            self.cached_mods = res.mods;
                            let mods_legacy = crate::pp::calculator::parse_mods_bits(res.mods);
                            let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
                            crate::beatmap::populate_beatmap_statistics_with_diff(
                                &mut self.cached_packet.beatmap,
                                map,
                                &diff,
                                res.mods,
                            );
                            self.cached_packet.beatmap.stats.stars.live =
                                self.cached_packet.beatmap.stats.stars.total;
                            self.cached_stats = self.cached_packet.beatmap.stats.clone();
                            self.cached_accuracy =
                                crate::pp::calculator::calc_accuracy_table_from_diff(&diff);
                            self.cached_graph = performance_graph(
                                map,
                                res.mods,
                                self.cached_packet.beatmap.time.first_object,
                                self.cached_packet.beatmap.time.last_object,
                                self.cached_packet.beatmap.time.mp3_length,
                            );
                            self.cached_difficulty_attrs = Some(diff);
                            self.cached_packet.performance.accuracy = self.cached_accuracy.clone();
                            self.cached_packet.performance.graph = self.cached_graph.clone();
                        }
                    }
                    if self.cached_packet.play.health_bar.normal <= 0.0 {
                        self.cached_packet.play.health_bar.normal = 100.0;
                        self.cached_packet.play.health_bar.smooth = 100.0;
                    }
                    let existing_hit_errors =
                        std::mem::take(&mut self.cached_packet.play.hit_error_array);
                    self.cached_packet.play.hit_error_array = if !self.enable_hit_errors {
                        Arc::default()
                    } else if existing_hit_errors.is_empty() {
                        let hit_count =
                            (res.hit_300 + res.hit_100 + res.hit_50 + res.hit_miss).max(0) as usize;
                        let spinner_count =
                            self.cached_packet.beatmap.stats.objects.spinners.max(0) as usize;
                        vec![0i16; hit_count.saturating_sub(spinner_count)].into()
                    } else {
                        existing_hit_errors
                    };

                    #[cfg(feature = "pp")]
                    if self.enable_pp {
                        if let Some(map) = &self.cached_beatmap {
                            let results_hits = (
                                res.max_combo as u32,
                                res.hit_300 as u32,
                                res.hit_100 as u32,
                                res.hit_50 as u32,
                                res.hit_miss as u32,
                                res.mods,
                            );
                            if self.cached_results_hits != results_hits
                                || self.cached_results_pp.is_none()
                            {
                                self.cached_results_hits = results_hits;
                                let mods_legacy = crate::pp::calculator::parse_mods_bits(res.mods);
                                // A finished play, so the curve is walked to the
                                // end once and thrown away -- there is no next
                                // judgement to make it incremental for. It is
                                // still the real curve, so `results_screen.pp.current`
                                // is the rating of the objects actually played rather
                                // than whichever chunk the hit count landed in.
                                let judged = res.hit_300 as u32
                                    + res.hit_100 as u32
                                    + res.hit_50 as u32
                                    + res.hit_miss as u32;
                                let mut cursor = crate::pp::calculator::GradualCursor::new();
                                let live_attrs = cursor.advance_to(
                                    self.cached_packet.beatmap.id as u32,
                                    map,
                                    mods_legacy,
                                    judged,
                                );
                                let full = crate::pp::calculator::full_difficulty(
                                    self.cached_packet.beatmap.id as u32,
                                    map,
                                    mods_legacy,
                                );
                                let live_res = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                                    live_attrs,
                                    &full,
                                    mods_legacy,
                                    res.max_combo as u32,
                                    res.hit_300 as u32,
                                    res.hit_100 as u32,
                                    res.hit_50 as u32,
                                    res.hit_miss as u32,
                                );
                                self.cached_results_pp = Some(live_res);
                            }
                            if let Some(live_res) = &self.cached_results_pp {
                                self.cached_packet.play.pp = live_res.clone();
                                self.cached_packet.results_screen.pp.current = live_res.current;
                                self.cached_packet.results_screen.pp.fc = live_res.fc;
                            }
                        }
                    }
                }
            }
        }

        // PP outside of active gameplay. The playing branch above owns PP while
        // osu! is in state 2, and the results branch owns state 7, so this only
        // fills in the menu and song select instead of reporting zeros. Cached on
        // (beatmap, mods) because nothing here changes between ticks.
        #[cfg(feature = "pp")]
        if self.enable_pp
            && current_state_num != 2
            && current_state_num != 7
            && let Some(map) = self.cached_beatmap.as_ref()
        {
            let beatmap_id = self.cached_packet.beatmap.id as u32;
            let mods_bits = self.cached_packet.play.mods.number;

            if self.cached_idle_pp_key != Some((beatmap_id, mods_bits)) {
                let mods_legacy = crate::pp::calculator::parse_mods_bits(mods_bits);
                let full = crate::pp::calculator::full_difficulty(beatmap_id, map, mods_legacy);
                // No objects judged outside gameplay, so `live_attrs` is `None`:
                // current and maxAchieved stay 0 while fc is the real value. The
                // whole-map rating is the only thing asked for here, so the curve
                // is not walked at all.
                let idle_pp = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                    None,
                    &full,
                    mods_legacy,
                    0,
                    0,
                    0,
                    0,
                    0,
                );
                self.cached_packet.play.pp = idle_pp;
                self.cached_idle_pp_key = Some((beatmap_id, mods_bits));
            }
        }
        // Entering gameplay or the results screen must not leave a stale menu
        // PP in place, and the results screen must keep its own PP.
        #[cfg(feature = "pp")]
        if self.enable_pp && (current_state_num == 2 || current_state_num == 7) {
            self.cached_idle_pp_key = None;
        }

        crate::instr_scope!(PacketClone);
        Ok(self.cached_packet.clone())
    }
}

/// Whether entering `next_state` should reset the play-derived fields.
///
/// tosu resets its gameplay and result-screen state objects on entry to song
/// select and again in the switch's `default:` arm, but its `case GameState.menu`
/// does nothing at all, so the main menu keeps the last play frozen rather than
/// zeroed. Match that: clearing on the way to state 0 would report values that
/// osu! and tosu both still report.
///
/// `play_state_dirty` is tosu's `isDefaultState` latch: it is set once gameplay
/// has actually been read, so nothing is cleared for a client that was never in
/// a map, and the reset runs once rather than every tick.
/// What one beatmap read means for the session caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeatmapReadAction {
    /// Not finished. Publish nothing, leave both caches unpinned, retry next tick.
    Retry,
    /// A different map: read its file, then commit only if the read worked.
    Load,
    /// The map already held: restore its file metadata from cache.
    Restore,
}

/// Classify a beatmap read against the state currently held.
///
/// `live_id` is the difficulty id at +0xC8 and `cached_id` the one already held.
/// The checksum alone cannot tell "same map" from "a different map whose md5 has
/// not landed yet" -- both present the previous map's md5 -- and guessing wrong
/// there is the stale-metadata bug, so the id breaks the tie. The id is stable for
/// a given map and difficulty, so `live_id != cached_id` means a different map.
fn beatmap_read_action(
    title: &str,
    checksum: &str,
    current_checksum: &str,
    live_id: i32,
    cached_id: i32,
) -> BeatmapReadAction {
    // A title alone is not a finished beatmap.
    if title.is_empty() || checksum.is_empty() {
        return BeatmapReadAction::Retry;
    }
    if checksum != current_checksum {
        return BeatmapReadAction::Load;
    }
    if live_id > 0 && cached_id > 0 && live_id != cached_id {
        return BeatmapReadAction::Load;
    }
    BeatmapReadAction::Restore
}

/// Whether the session caches may advance past this tick.
///
/// `Retry` never advances, which is what makes the retry unbounded.
///
/// `Load` advances only if the map's own file was actually read. A transient lock
/// at the moment of the switch -- an AV scanner, OneDrive, the osu! editor -- used
/// to commit the checksum and the beatmap id against a read that had no stars, no
/// object counts and no max combo, and the map then stayed that way for its whole
/// duration. That is a different failure from the stale-metadata one and produces
/// the same user-visible report, distinguishable only by the checksum being right.
fn caches_may_advance(read: BeatmapReadAction, metadata_ok: bool, file_ok: bool) -> bool {
    match read {
        BeatmapReadAction::Retry => false,
        BeatmapReadAction::Restore => true,
        BeatmapReadAction::Load => metadata_ok && file_ok,
    }
}

/// Re-apply the beatmap file's own ruleset from a cached file-metadata snapshot.
///
/// The file pass (`populate_beatmap_file_metadata` then
/// `apply_beatmap_ruleset`) runs on exactly one tick, because the checksum gate
/// in front of it only opens when the map changes. Every later tick restores the
/// file's metadata from a cache instead, and that restore originally copied
/// source, tags, objects, times and bpm -- **not** `mode`, `is_convert` or
/// `file_mode`. So `beatmap.mode` was the file's ruleset for one tick and the
/// game's current ruleset for the rest, and `isConvert` was permanently
/// `false` again from the second frame onward. In tournament mode the
/// snapshot being restored is itself the pre-file-pass clone, so the wrong value
/// is not transient there: it is the steady state.
///
/// `target.current_ruleset` is the game's current ruleset -- the value read from
/// `base_addr - 0x33` and never overwritten, so it is available here however
/// many restores have run. Reading it out of `target.mode.number` would work
/// only on the first pass, since the file pass is what overwrites `mode`.
///
/// Guarded on `file_mode.is_some()` on purpose: an absent `Mode:` line means
/// osu!standard *to the parser*, but reaching here with no file read at all
/// means there is nothing to restore, and `apply_beatmap_ruleset` would resolve
/// `None` to `0` and replace a real current ruleset with a guess.
fn restore_beatmap_ruleset(
    target: &mut crate::beatmap::BeatmapSnapshot,
    metadata: &crate::beatmap::BeatmapSnapshot,
) {
    let Some(file_mode) = metadata.file_mode else {
        return;
    };
    target.file_mode = Some(file_mode);
    crate::beatmap::apply_beatmap_ruleset(target, target.current_ruleset);
}

/// tosu's `game.paused`, as a rule over two samples of the song clock.
///
/// `states/global.ts:68-69` is the whole of it upstream:
///
/// ```ts
/// this.paused = this.previousPlayTime === this.playTime;
/// this.previousPlayTime = this.playTime;
/// ```
///
/// So the value is a *comparison across ticks*, not a read, and it is not
/// derived from the game state at all. The previous code compared the osu!
/// **game state** against 7, which is `resultScreen` -- so it reported `paused`
/// on the results screen and `not paused` for a paused map, which is the exact
/// inverse of what it means.
///
/// The clock is the song position (`beatmap.time.live`, tosu's `global.playTime`
/// fed by the 10 ms `globalPrecise` loop), *not* `session.playTime`, which is
/// `global.gameTime`: a session-wide counter that keeps advancing while the song
/// is frozen.
///
/// `previous` is `None` on the first tick. tosu seeds both counters at 0, so its
/// first tick reports `paused: true`; reporting `false` is the honest answer for
/// a clock that has only been read once, and the state lasts a single tick.
fn is_paused(previous: Option<i32>, live: i32) -> bool {
    previous == Some(live)
}

fn should_clear_play_state(play_state_dirty: bool, next_state: i32) -> bool {
    // 0 menu, 2 play and 7 resultScreen are excluded, and so are 11 lobby,
    // 12 matchSetup and 15 onlineSelection: tosu breaks on those three with the
    // comment "do not spam reset on multiplayer and direct"
    // (`instances/osuInstance.ts:186-190`), deliberately keeping the last play
    // frozen. GameState is numbered at `common/enums/osu.ts:13-40`.
    play_state_dirty && !matches!(next_state, 0 | 2 | 7 | 11 | 12 | 15)
}

/// Drop every play-derived field so a finished attempt cannot leak into the
/// next view of the client. Runs both on a beatmap checksum change and on
/// leaving gameplay, mirroring tosu's `gameplay.init()` and
/// `resultScreen.init()`.
fn clear_play_state_for_new_map(packet: &mut crate::v2::TosuV2Packet) {
    let play = &mut packet.play;
    play.player_name.clear();
    play.failed = false;
    play.score = 0;
    // tosu's `GameplayState.init` sets accuracy to 100 for an empty play: an
    // unjudged play is perfect, not 0%. `resultScreen.init` uses 0, which is
    // why the results block below differs.
    play.accuracy = 100.0;
    play.health_bar = Default::default();
    play.hits = Default::default();
    play.hit_error_array = Arc::default();
    play.combo = Default::default();
    play.rank = Default::default();
    play.unstable_rate = 0.0;
    play.pp = Default::default();
    // tosu's `gameplay.resetKeyOverlay()`, which its precise loop calls in every
    // state except `play` (`osuInstance.ts:234-237`). Resetting the buttons
    // without resetting the rest of the play block is deliberate: the frozen
    // last play is tosu's behaviour for the *lobby/match-setup/online-selection*
    // states, whereas the key overlay is reset in **all** of them.
    play.key_overlay = Default::default();

    let results = &mut packet.results_screen;
    results.player_name.clear();
    results.score = 0;
    results.accuracy = 0.0;
    results.hits = Default::default();
    results.mods = Default::default();
    results.max_combo = 0;
    results.rank.clear();
    results.pp.current = 0.0;
    results.pp.fc = 0.0;
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "pp")]
    use super::performance_graph;
    use super::{
        FILE_RETRY_INITIAL, FILE_RETRY_MAX, LEADERBOARD_INTERVAL, clear_play_state_for_new_map,
        file_attempt_due, is_paused, note_file_load_failure, restore_beatmap_ruleset,
        should_clear_play_state,
    };
    use crate::v2::TosuV2Packet;
    use std::time::{Duration, Instant};

    /// The score list is walked on a 1 Hz timer rather than on every tick.
    ///
    /// This is a rate limit, not a cache of a fixed size, so the property worth
    /// pinning is the interval itself: a second is long enough to collapse the
    /// 60 reads a second into one, and short enough that a score moving up the
    /// board is not visibly stale. A regression to "every tick" (the bug this
    /// replaced) or to something coarser than a second would both be wrong.
    #[test]
    fn the_leaderboard_is_re_read_on_a_one_second_timer() {
        assert_eq!(
            LEADERBOARD_INTERVAL,
            Duration::from_secs(1),
            "the score list is re-walked at 1 Hz"
        );
        // The backdating the two reset sites rely on: subtracting the interval
        // must make the very next read due, or a freshly loaded map would sit
        // with an empty leaderboard for up to a second.
        let just_reset = Instant::now() - LEADERBOARD_INTERVAL;
        assert!(
            just_reset.elapsed() >= LEADERBOARD_INTERVAL,
            "a reset must leave the next read immediately due"
        );
    }

    /// A finished attempt, as the provider would hold it between plays.
    fn played_packet() -> TosuV2Packet {
        let mut packet = TosuV2Packet::default();
        packet.play.player_name = "player".to_string();
        packet.play.score = 1_234_567;
        packet.play.accuracy = 98.7654;
        packet.play.combo.current = 412;
        packet.play.combo.max = 1103;
        packet.play.hits.n300 = 1502;
        packet.play.hits.n100 = 143;
        packet.play.hits.n50 = 12;
        packet.play.hits.n0 = 7;
        packet.play.rank.current = "S".to_string();
        packet.play.unstable_rate = 12.3456;
        packet.play.health_bar.normal = 42.0;
        packet.play.failed = true;
        packet.play.pp.current = 812.5;
        packet.play.pp.fc = 1502.25;
        packet.results_screen.score = 1_234_567;
        packet.results_screen.rank = "S".to_string();
        packet.results_screen.max_combo = 1103;
        packet.results_screen.pp.current = 812.5;
        packet.results_screen.pp.fc = 1502.25;
        packet
    }

    /// Replay a state sequence through the latch, returning one clear decision
    /// per state after the first. The leading state is the one that arms the
    /// latch, and 2 or 7 can never itself clear, so it carries no decision.
    fn clear_decisions(states: &[i32]) -> Vec<bool> {
        let mut dirty = false;
        let mut decisions = Vec::new();
        for (index, next) in states.iter().enumerate() {
            let clear = should_clear_play_state(dirty, *next);
            if index > 0 {
                decisions.push(clear);
            }
            // A state that reads gameplay or a results screen re-arms the
            // latch; only a clear drops it again.
            if matches!(*next, 2 | 7) {
                dirty = true;
            }
            if clear {
                dirty = false;
            }
        }
        decisions
    }

    #[test]
    fn clear_matrix_pins_tosus_state_handling() {
        let cases: &[(bool, i32, bool)] = &[
            (true, 5, true),
            (true, 4, true),
            (true, 0, false),
            (true, 2, false),
            (true, 7, false),
            (true, 3, true),
            (true, 6, true),
            (true, 8, true),
            (true, 13, true),
            (false, 5, false),
            (false, 0, false),
        ];

        for (dirty, next_state, expected) in cases {
            assert_eq!(
                should_clear_play_state(*dirty, *next_state),
                *expected,
                "dirty={dirty} next_state={next_state}"
            );
        }
    }

    /// State 0 is the one case a "sensible" implementation gets wrong. tosu's
    /// `case GameState.menu` only calls `bassDensity.updateState()` and breaks,
    /// so the main menu keeps the last play frozen instead of zeroing it.
    /// Clearing here would report values osu! and tosu both still report.
    #[test]
    fn main_menu_freezes_the_last_play_instead_of_clearing_it() {
        assert!(!should_clear_play_state(true, 0));
    }

    /// `game.paused` is a comparison across ticks, not a game-state test.
    ///
    /// The bug it replaces read the osu! state and compared it to 7, which is
    /// `resultScreen` -- so a paused map reported `paused: false` and a results
    /// screen reported `paused: true`. Both arms are pinned here, plus the
    /// first-tick case, where there is no previous sample yet.
    #[test]
    fn paused_is_two_equal_song_clock_samples() {
        // The map is paused: the song position is not moving.
        assert!(is_paused(Some(11_575), 11_575));
        assert!(is_paused(Some(11_575), 11_575));
        // The map is playing: the song position advanced.
        assert!(!is_paused(Some(11_575), 11_592));
        // Only one sample so far. tosu would say `true` here (both of its
        // counters start at 0); a single reading has not observed a pause.
        assert!(!is_paused(None, 11_575));
    }

    #[test]
    fn the_latch_clears_only_once_across_a_repeated_state() {
        assert_eq!(clear_decisions(&[2, 5, 5]), vec![true, false]);
    }

    /// Quitting to the main menu mid-lifecycle must not consume the clear: the
    /// latch stays armed there, so the next song-select tick is the one that
    /// drops the run. A prev/next transition table keyed on 2 -> 0 would clear
    /// early and leave the second arrival doing nothing.
    #[test]
    fn a_visit_to_the_main_menu_does_not_consume_the_pending_clear() {
        assert_eq!(clear_decisions(&[2, 0, 5]), vec![false, true]);
    }

    #[test]
    fn new_map_clears_previous_play_state() {
        let mut packet = played_packet();
        clear_play_state_for_new_map(&mut packet);

        assert_eq!(packet.play.score, 0);
        // An empty play is perfect: tosu's `GameplayState.init` sets accuracy to
        // 100, not 0, because nothing has been judged yet.
        assert_eq!(packet.play.accuracy, 100.0);
        assert_eq!(packet.play.combo.current, 0);
        assert_eq!(packet.play.combo.max, 0, "max combo is per map");
        assert_eq!(packet.play.hits.n300, 0);
        assert_eq!(packet.play.hits.n100, 0);
        assert_eq!(packet.play.hits.n50, 0);
        assert_eq!(packet.play.hits.n0, 0);
        assert_eq!(packet.play.rank.current, "");
        assert_eq!(packet.play.unstable_rate, 0.0);
        assert_eq!(packet.play.health_bar.normal, 0.0);
        assert!(!packet.play.failed);
        assert!(packet.play.player_name.is_empty());
    }

    #[test]
    fn new_map_clears_the_results_screen() {
        let mut packet = played_packet();
        clear_play_state_for_new_map(&mut packet);

        assert_eq!(packet.results_screen.score, 0);
        assert_eq!(packet.results_screen.rank, "");
        assert_eq!(packet.results_screen.max_combo, 0);
        // tosu's `resultScreen.init` uses 0 here, unlike gameplay's 100.
        assert_eq!(packet.results_screen.accuracy, 0.0);
    }

    #[test]
    fn new_map_keeps_the_beatmap_and_mods() {
        let mut packet = played_packet();
        packet.beatmap.set = 4242;
        packet.beatmap.title = "kept".to_string();
        packet.play.mods.number = 40;
        packet.play.mods.name = "HDDT".to_string();
        clear_play_state_for_new_map(&mut packet);

        // Mods are re-read from memory and PP is recalculated per map, so both
        // must survive; the map metadata is unrelated to the previous attempt.
        assert_eq!(packet.beatmap.set, 4242);
        assert_eq!(packet.beatmap.title, "kept");
        assert_eq!(packet.play.mods.number, 40);
        assert_eq!(packet.play.mods.name, "HDDT");
    }

    /// Leaving the map shares the clear, and song select is the one place the
    /// player still needs a beatmap and the mods they queued with. Dropping
    /// either would blank the song-select header.
    #[test]
    fn leaving_play_keeps_the_beatmap_and_mods() {
        let mut packet = played_packet();
        packet.beatmap.set = 4242;
        packet.beatmap.title = "kept".to_string();
        packet.play.mods.number = 40;
        packet.play.mods.name = "HDDT".to_string();
        clear_play_state_for_new_map(&mut packet);

        assert_eq!(packet.beatmap.set, 4242);
        assert_eq!(packet.beatmap.title, "kept");
        assert_eq!(packet.play.mods.number, 40);
        assert_eq!(packet.play.mods.name, "HDDT");
    }

    #[test]
    fn clear_zeroes_play_pp_and_the_whole_results_screen() {
        let mut packet = played_packet();
        clear_play_state_for_new_map(&mut packet);

        // The idle PP path refills this with a zeroed current against a real fc
        // for the highlighted map, which is what tosu's
        // `beatmapPP.resetAttributes()` leaves behind.
        assert_eq!(packet.play.pp.current, 0.0);
        assert_eq!(packet.play.pp.fc, 0.0);
        assert_eq!(packet.results_screen.pp.current, 0.0);
        assert_eq!(packet.results_screen.pp.fc, 0.0);
        assert!(packet.results_screen.player_name.is_empty());
        assert_eq!(packet.results_screen.mods.number, 0);
        assert_eq!(packet.results_screen.hits.n300, 0);
    }

    // ---------------------------------------------------------------------
    // Beatmap switching. audit-1.0.5.md E-01, E-02, E-03, E-05.
    //
    // osu! fills `BeatmapInfo` in field by field, so mid-switch the title and the
    // id at +0xC8 can already be current while the md5 at +0x6C is still the
    // previous map's, or empty. tosu treats such a read as 'not-ready' and skips
    // the whole state update (`states/beatmap.ts:355-377`,
    // `instances/osuInstance.ts:113`), which leaves the previously published
    // beatmap standing and retries on the next tick.
    //
    // Reported symptom, in both forms: after selecting a new map the overlay kept
    // the previous map's stars, object counts, source and tags, while showing the
    // new map's title. Two distinct causes, told apart by the checksum.
    // ---------------------------------------------------------------------

    use super::{BeatmapReadAction, beatmap_read_action, caches_may_advance};

    const PREV_MD5: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// The whole classification table, so a change to any rule is visible here
    /// rather than only in whichever test happened to exercise it.
    #[test]
    fn beatmap_read_actions_cover_the_switch_matrix() {
        // title, checksum, held checksum, live id, held id -> action
        let cases: &[(bool, &str, &str, i32, i32, BeatmapReadAction)] = &[
            // Nothing yet: osu! cleared the pointer, or the record is empty.
            (false, "", "", 0, 0, BeatmapReadAction::Retry),
            // Title landed, md5 has not. Publishes nothing, retries.
            (true, "", PREV_MD5, 2964306, 1, BeatmapReadAction::Retry),
            // A clean new map.
            (
                true,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                PREV_MD5,
                2964306,
                1,
                BeatmapReadAction::Load,
            ),
            // The same map, re-read: restore the file metadata from cache.
            (true, PREV_MD5, PREV_MD5, 1, 1, BeatmapReadAction::Restore),
            // A different map still carrying the previous map's md5. This is the
            // case the old code got wrong: the checksum matched, so it took the
            // restore branch and stamped the previous map's stats onto the new
            // one. The id is what distinguishes it.
            (
                true,
                PREV_MD5,
                PREV_MD5,
                2964306,
                1,
                BeatmapReadAction::Load,
            ),
            // First ever read: nothing held, so there is nothing to restore.
            (true, PREV_MD5, "", 2964306, 0, BeatmapReadAction::Load),
            // An unresolved id must not manufacture a difference.
            (true, PREV_MD5, PREV_MD5, 0, 1, BeatmapReadAction::Restore),
            (true, PREV_MD5, PREV_MD5, 1, 0, BeatmapReadAction::Restore),
        ];

        for (has_title, checksum, held, live_id, cached_id, expected) in cases {
            let title = if *has_title { "New Map" } else { "" };
            assert_eq!(
                beatmap_read_action(title, checksum, held, *live_id, *cached_id),
                *expected,
                "title={has_title} checksum={checksum} held={held} live_id={live_id} cached_id={cached_id}"
            );
        }
    }

    /// The regression in one assertion: a new map whose md5 has not landed must
    /// never reach the cache-restore path.
    #[test]
    fn a_new_map_whose_md5_has_not_landed_is_never_restored_from_cache() {
        let held = PREV_MD5;
        let unresolved = beatmap_read_action("New Map", "", held, 2964306, 1);
        assert_eq!(unresolved, BeatmapReadAction::Retry);

        let stale_md5 = beatmap_read_action("New Map", held, held, 2964306, 1);
        assert_ne!(
            stale_md5,
            BeatmapReadAction::Restore,
            "a different difficulty id with a matching md5 is a different map"
        );
    }

    /// A read that could not be loaded must not advance the caches, or the map is
    /// latched with no stars, no object counts and no max combo for its whole
    /// duration. This is the second cause of the same report, and the one a
    /// user is more likely to hit: an AV scanner, OneDrive or the osu! editor
    /// holding the `.osu` at the instant of the switch is enough.
    #[test]
    fn a_map_whose_file_could_not_be_read_is_retried_rather_than_latched() {
        let load = BeatmapReadAction::Load;

        assert!(
            caches_may_advance(load, true, true),
            "a clean load advances"
        );
        assert!(
            !caches_may_advance(load, false, true),
            "a failed metadata read must not commit the checksum"
        );
        assert!(
            !caches_may_advance(load, true, false),
            "a failed file read must not commit the checksum"
        );
        assert!(!caches_may_advance(load, false, false));

        // An unresolved read never advances, whatever the load result claims,
        // which is what makes the retry unbounded.
        for (meta, file) in [(true, true), (false, false), (true, false)] {
            assert!(!caches_may_advance(BeatmapReadAction::Retry, meta, file));
        }

        // Restoring the map already held changes nothing, so it always advances.
        assert!(caches_may_advance(BeatmapReadAction::Restore, false, false));
    }

    /// tosu breaks out of the lobby, match-setup and online-selection arms with
    /// "do not spam reset on multiplayer and direct"
    /// (`instances/osuInstance.ts:186-190`), so the last play stays frozen there
    /// rather than being zeroed. GameState is numbered at
    /// `common/enums/osu.ts:13-40`: lobby 11, matchSetup 12, onlineSelection 15.
    #[test]
    fn the_multiplayer_lobby_freezes_the_last_play_like_tosu() {
        for (state, name) in [(11, "lobby"), (12, "matchSetup"), (15, "onlineSelection")] {
            assert!(
                !should_clear_play_state(true, state),
                "tosu keeps the last play in {name} (state {state})"
            );
        }
        // The states that do clear, unchanged.
        for state in [3, 4, 5, 6, 8, 13, 14, 22] {
            assert!(
                should_clear_play_state(true, state),
                "state {state} should clear"
            );
        }
    }

    /// A `.osu` file that will not load is retried, but not sixty times a second.
    ///
    /// The correctness requirement is unchanged and comes first: a map whose file
    /// could not be read must never be latched, or a previous map's tags and
    /// object counts would sit on top of it. What changed is only the *rate* at
    /// which the attempt is made, because both disk operations on that path --
    /// `read_to_string` plus a line-by-line parse, and `fs::read` plus
    /// `Beatmap::from_bytes` -- used to run on every tick for the whole time the
    /// map was selected, and a tournament client selecting a map it has not
    /// downloaded is an ordinary situation rather than an edge case.
    #[test]
    fn a_file_that_will_not_load_is_retried_on_a_backoff_not_every_tick() {
        // Nothing has failed yet, so any map may be read.
        assert!(file_attempt_due(&None, 0x1000));

        let mut state: Option<(u64, Instant, Duration)> = None;
        note_file_load_failure(&mut state, 0x1000);
        assert!(
            !file_attempt_due(&state, 0x1000),
            "the map that just failed is not re-read on the same tick"
        );

        // A *different* map is never delayed by an earlier map's backoff.
        assert!(
            file_attempt_due(&state, 0x2000),
            "a newly selected map is read immediately"
        );

        // Consecutive failures on the same map back off, and stop at the ceiling.
        // Reset first, so the sequence below starts from a clean first failure.
        state = None;
        let mut delays = Vec::new();
        for _ in 0..8 {
            note_file_load_failure(&mut state, 0x1000);
            delays.push(state.expect("recorded").2);
        }
        assert_eq!(delays[0], FILE_RETRY_INITIAL);
        assert_eq!(delays[1], FILE_RETRY_INITIAL * 2);
        assert_eq!(*delays.last().expect("non-empty"), FILE_RETRY_MAX);
        assert!(
            delays.windows(2).all(|pair| pair[1] >= pair[0]),
            "the backoff never shrinks while the same map keeps failing"
        );
        assert!(!file_attempt_due(&state, 0x1000), "still backing off");

        // Success clears it, so a map that becomes readable is picked up at once
        // rather than waiting out the backoff.
        state = None;
        assert!(file_attempt_due(&state, 0x1000));
    }

    /// A cached file-metadata snapshot must carry the map's own ruleset and the
    /// conversion flag forward, not just its tags and timings.
    ///
    /// The file pass runs on exactly one tick, so every later tick restores from
    /// a cache instead. The restore originally copied source, tags, objects,
    /// times and bpm and stopped there, which meant `beatmap.mode` was the
    /// file's ruleset for one frame and the game's current ruleset for the rest,
    /// and `isConvert` was back to `false` from the second frame onward -- so in
    /// tournament mode, where the cached snapshot is itself the pre-file-pass
    /// clone, that wrong value was the steady state rather than a blip.
    #[test]
    fn restoring_a_cached_map_keeps_its_own_ruleset_and_the_conversion_flag() {
        // A converted osu!standard map being played in mania: the memory read
        // gives the current ruleset (3), the file says 0.
        let mut live = crate::beatmap::BeatmapSnapshot {
            current_ruleset: 3,
            ..Default::default()
        };
        live.mode = crate::beatmap::BeatmapMode {
            number: 3,
            name: "mania".to_string(),
        };

        // What the file pass cached: the map's own ruleset, already applied.
        let mut metadata = live.clone();
        metadata.file_mode = Some(0);
        crate::beatmap::apply_beatmap_ruleset(&mut metadata, 3);

        // A later tick restores from the cache onto a fresh memory read, which
        // is back to reporting the current ruleset.
        let mut restored = live.clone();
        assert_eq!(
            restored.mode.number, 3,
            "precondition: the read's own value"
        );
        assert!(!restored.is_convert, "precondition: no file pass yet");

        restore_beatmap_ruleset(&mut restored, &metadata);

        assert_eq!(
            restored.mode.number, 0,
            "the map's own ruleset survives the restore"
        );
        assert_eq!(restored.mode.name, "osu");
        assert!(
            restored.is_convert,
            "isConvert is recomputed from the current ruleset, not left false"
        );

        // The current ruleset itself is never clobbered, because `isConvert` and
        // v1's `menu.gameMode` both need it after the overwrite.
        assert_eq!(restored.current_ruleset, 3);

        // Restoring twice is stable: the second pass must not read the already
        // overwritten `mode` as if it were the current ruleset.
        restore_beatmap_ruleset(&mut restored, &metadata);
        assert_eq!(restored.mode.number, 0);
        assert!(restored.is_convert, "still converted on the second pass");
    }

    /// A restore with no cached `Mode:` must leave the current ruleset alone.
    ///
    /// `apply_beatmap_ruleset` resolves an absent `Mode:` to osu!standard, which
    /// is right for a file that was actually parsed. Reaching here with no file
    /// read at all is a different thing, and applying it would replace a real
    /// current ruleset with a guess.
    #[test]
    fn restoring_without_a_parsed_mode_does_not_guess_one() {
        let mut target = crate::beatmap::BeatmapSnapshot {
            current_ruleset: 2,
            ..Default::default()
        };
        target.mode = crate::beatmap::BeatmapMode {
            number: 2,
            name: "fruits".to_string(),
        };

        let metadata = crate::beatmap::BeatmapSnapshot::default();

        restore_beatmap_ruleset(&mut target, &metadata);

        assert_eq!(target.mode.number, 2, "the current ruleset stands");
        assert_eq!(target.mode.name, "fruits");
        assert!(!target.is_convert);
    }

    /// The `reading` graph series is **populated**, and is not one of the three
    /// shapes it has previously been.
    ///
    /// `rosu-pp-gemini` 5.0.3 added `OsuStrains::reading`; before that the skill
    /// computed no sections, so rtosu emitted `[]` and pinned it. The series is
    /// now real data, and the properties worth pinning are the ones that survived
    /// every earlier mistake:
    ///
    /// * **populated** -- a real non-zero value exists on a map with real reading
    ///   content, so the old empty array now *fails*.
    /// * **section-shaped** -- it has one entry per 400 ms section, the same count
    ///   as `speed` and `flashlight`. This is why it indexes the shared x-axis
    ///   without reshaping. `aim` deliberately is not the comparison: its sections
    ///   are variable-length in the crate, so its length is not a fixed 400 ms grid.
    /// * **not the aim series** -- the 1.0.4 defect, where an overlay drawing the
    ///   reading graph was drawing the aim graph under the wrong name.
    /// * **not flat zeros** -- the 1.0.5 behaviour. A flat 0.0 line passes an
    ///   "is it aim?" check, so it needs its own: it is a claim about the map
    ///   indistinguishable from a genuinely 0-strain map.
    /// * **still present** -- tosu builds five series for osu!std
    ///   (`states/beatmap.ts:727-734`: `aim, aimNoSliders, reading, flashlight,
    ///   speed`), so dropping the key would change the shape for every consumer
    ///   that indexes by name or position.
    #[test]
    #[cfg(feature = "pp")]
    fn the_reading_series_is_populated_and_is_neither_an_aim_clone_nor_flat_zeros() {
        use rosu_pp::Beatmap;

        // The fixture has to be a map with something to *read*, or the assertion
        // below is vacuous. Two properties of the reading evaluator decide it:
        //
        // * `velocity` is `lazy_jump_dist / delta_time` floored at 1.0, so circles
        //   that never move contribute the floor and nothing more;
        // * `get_constant_angle_nerf_factor` collapses to its 0.2 floor for a
        //   pattern that keeps the same angle -- a straight, evenly spaced stream
        //   of circles at one position is the most heavily nerfed shape there is.
        //
        // So: scattered positions and irregular deltas, from a seeded LCG so the
        // fixture is the same map on every run.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as u32
        };

        let mut content = String::from(
            "osu file format v14\n\n[General]\nMode: 0\n\n[Metadata]\nTitle:Test\nArtist:Test\nCreator:Test\nVersion:Normal\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:9\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n",
        );
        let mut time = 1000u32;
        for _ in 0..300 {
            let x = 64 + next() % 384;
            let y = 64 + next() % 256;
            content.push_str(&format!("{x},{y},{time},1,0,0:0:0:0:\n"));
            time += 130 + next() % 200;
        }
        let map = Beatmap::from_bytes(content.as_bytes()).expect("parse map");

        let graph = performance_graph(&map, 0, 0, 0, 30_000);
        let graph: crate::v2::PerformanceGraph =
            serde_json::from_str(graph.raw.get()).expect("graph decodes");

        let names: Vec<&str> = graph.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["aim", "aimNoSliders", "reading", "flashlight", "speed"],
            "the five osu!std series and their order are tosu's (beatmap.ts:727-734)"
        );

        let series = |name: &str| -> Vec<f64> {
            graph
                .series
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("the {name} key is present"))
                .data
                .clone()
        };
        let reading = series("reading");
        let speed = series("speed");
        let aim = series("aim");

        // Populated, and padded to the shared x-axis like every other series.
        assert_eq!(
            reading.len(),
            graph.xaxis.len(),
            "every series must index the same x-axis, got {} vs {}",
            reading.len(),
            graph.xaxis.len()
        );

        // Section-shaped: one entry per 400 ms, matching the other fixed-grid
        // skill. `aim` is excluded on purpose -- its sections are variable-length
        // in the crate, so its length is not a fixed grid to compare against.
        assert_eq!(
            reading.len(),
            speed.len(),
            "reading and speed are both fixed 400 ms sections"
        );

        // Not empty, and not a flat line. Both were real past behaviours.
        let unpadded = reading
            .iter()
            .copied()
            .filter(|&v| v != -100.0)
            .collect::<Vec<f64>>();
        assert!(
            !unpadded.is_empty(),
            "the old empty array must now fail: reading carries no samples"
        );
        assert!(
            unpadded.iter().any(|&v| v > 0.0),
            "a flat 0.0 line is a claim about the map indistinguishable from a \
             genuinely 0-strain one; got {unpadded:?}"
        );

        // And it is not the aim series wearing the wrong name, which is the
        // defect 1.0.4 found here.
        assert_ne!(reading, aim, "reading must never be an aim clone");
    }

    /// The live evidence for a counter that is not a hard 0, modelled on what
    /// was actually measured on map 4390203: tosu on :24050 reported 3 slider
    /// breaks against rtosu's 0, and rtosu had attached 176 objects into the map
    /// (combo 7, max combo 124, 3 misses).
    ///
    /// The 3 were unreachable -- osu!stable keeps no slider-break statistic, so
    /// there is nothing in memory to read and the counts are not on a common
    /// scale (176 judged vs a max combo of 124, because the hit counters include
    /// slider ticks and the combo counter does not). What *is* in reach is
    /// everything after the attach, and this walks it.
    #[test]
    fn the_counter_starts_counting_from_the_moment_of_attach() {
        let (mut breaks, mut prev_combo, mut prev_miss, mut prev_max_combo) =
            (0i32, 0i32, 0i32, 0i32);
        let mut step = |combo: i16, max_combo: i16, miss: i16| {
            super::infer_slider_breaks(
                &mut prev_combo,
                &mut prev_miss,
                &mut prev_max_combo,
                &mut breaks,
                combo,
                max_combo,
                miss,
            )
        };

        // The first poll after attaching mid-map. Whatever happened before this
        // is gone, and the counter is honest about it: 0, not a guess.
        assert_eq!(step(7, 124, 3), 0);

        // A real slider break: combo drops, misses do not move.
        assert_eq!(step(0, 124, 3), 1);

        // Combo rebuilds across several polls. A counter that latched after one
        // drop, or one that counted a drop per poll, would not survive this.
        assert_eq!(step(3, 124, 3), 1);
        assert_eq!(step(18, 124, 3), 1);
        assert_eq!(step(40, 124, 3), 1);

        // A second break.
        assert_eq!(step(0, 124, 3), 2);

        // A miss also zeroes the combo, and must not be mistaken for a break.
        assert_eq!(step(0, 124, 4), 2);
        assert_eq!(step(12, 124, 4), 2);
    }

    /// A combo threshold is a *divergence* from tosu, not a safety improvement.
    ///
    /// `updateSliderBreaks` has no threshold, so a slider break at combo 15 in
    /// the opening seconds of a map is counted by tosu. Requiring the pre-drop
    /// combo to be above some bound would report 0 where tosu reports 1, turning
    /// today's single structural gap into a second, avoidable one. Pinned so
    /// the trade-off is a decision rather than a later surprise.
    #[test]
    fn a_break_below_any_combo_threshold_still_counts() {
        let (mut breaks, mut prev_combo, mut prev_miss, mut prev_max_combo) =
            (0i32, 0i32, 0i32, 0i32);
        let mut step = |combo: i16, max_combo: i16, miss: i16| {
            super::infer_slider_breaks(
                &mut prev_combo,
                &mut prev_miss,
                &mut prev_max_combo,
                &mut breaks,
                combo,
                max_combo,
                miss,
            )
        };

        // Combo 8 -> 0 with no miss. Early in a map, and tosu counts it.
        assert_eq!(step(8, 8, 0), 0);
        assert_eq!(step(0, 8, 0), 1);
    }

    #[test]
    fn slider_breaks_are_inferred_from_combo_drop_without_miss_and_reset_on_retry() {
        // Driven through the free function rather than a session method, which
        // is how the poll path calls it. A method that existed only for these
        // assertions would be dead code in every build that is not a test.
        let (mut breaks, mut prev_combo, mut prev_miss, mut prev_max_combo) =
            (0i32, 0i32, 0i32, 0i32);
        let mut step = |combo: i16, max_combo: i16, miss: i16| {
            super::infer_slider_breaks(
                &mut prev_combo,
                &mut prev_miss,
                &mut prev_max_combo,
                &mut breaks,
                combo,
                max_combo,
                miss,
            )
        };

        // 1. Initially 0 breaks
        assert_eq!(step(0, 0, 0), 0);

        // 2. Combo increases cleanly -> 0 breaks
        assert_eq!(step(10, 10, 0), 0);
        assert_eq!(step(50, 50, 0), 0);

        // 3. Repeated polls with identical state must not double count
        assert_eq!(step(50, 50, 0), 0);
        assert_eq!(step(50, 50, 0), 0);

        // 4. Combo drop without miss -> 1 slider break
        assert_eq!(step(0, 50, 0), 1);

        // 5. Subsequent poll while still at 0 combo must not count again
        assert_eq!(step(0, 50, 0), 1);

        // 6. Combo builds back up -> still 1 break
        assert_eq!(step(20, 50, 0), 1);

        // 7. Combo drop accompanied by a miss -> NOT a slider break (still 1)
        assert_eq!(step(0, 50, 1), 1);

        // 8. Combo builds up to 15 -> still 1
        assert_eq!(step(15, 50, 1), 1);

        // 9. Second slider break: combo drops from 15 to 0 while miss remains 1
        assert_eq!(step(0, 50, 1), 2);

        // 10. Retry: max combo drops -> slider breaks reset to 0
        assert_eq!(step(0, 0, 0), 0);
        assert_eq!(step(5, 5, 0), 0);
    }

    #[test]
    #[cfg(feature = "pp")]
    fn beatmap_stats_builder_zeroes_live_stars_while_preserving_total() {
        use rosu_pp::Beatmap;

        let mut content = String::from(
            "osu file format v14\n\n[General]\nMode: 0\n\n[Metadata]\nTitle:Test\nArtist:Test\nCreator:Test\nVersion:Normal\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,0,50,1,0\n\n[HitObjects]\n",
        );
        for i in 0..50 {
            content.push_str(&format!("256,192,{},1,0,0:0:0:0:\n", 1000 + i * 100));
        }
        let map = Beatmap::from_bytes(content.as_bytes()).expect("parse map");
        let mods_legacy = crate::pp::calculator::parse_mods_bits(0);
        let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(&map);

        let mut snapshot = crate::beatmap::BeatmapSnapshot::default();
        crate::beatmap::populate_beatmap_statistics_with_diff(&mut snapshot, &map, &diff, 0);

        assert!(snapshot.stats.stars.total > 0.0);
        assert_eq!(snapshot.stats.stars.live, 0.0);
        assert_ne!(snapshot.stats.stars.live, snapshot.stats.stars.total);
    }
}
