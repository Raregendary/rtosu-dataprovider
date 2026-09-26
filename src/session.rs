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
    pub cached_metadata: Option<crate::beatmap::BeatmapSnapshot>,
    pub cached_total_hits: u32,
    pub cached_hit_errors: Arc<[i16]>,
    pub cached_unstable_rate: f64,
    pub cached_gameplay: Option<GameplayState>,
    pub cached_beatmap_ptr: u64,
    pub cached_beatmap_snapshot: Option<BeatmapSnapshot>,
    pub cached_user_ptr: u64,
    pub cached_user: Option<TournamentUser>,
    pub cached_chat_size: usize,
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
    pub gradual_pp_chunks: usize,
    pub enable_hit_errors: bool,
    pub current_checksum: String,
    #[cfg(feature = "pp")]
    pub cached_beatmap: Option<rosu_pp::Beatmap>,
    pub cached_metadata: Option<crate::beatmap::BeatmapSnapshot>,
    pub cached_stats: crate::beatmap::BeatmapStats,
    pub cached_stats_by_mods: HashMap<u32, crate::beatmap::BeatmapStats>,
    pub cached_accuracy: crate::v2::PerformanceAccuracy,
    pub cached_graph: crate::v2::PrecomputedGraph,
}

impl TournamentSession {
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
            gradual_pp_chunks: 100,
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

                std::thread::scope(|scope| {
                    for _ in 0..worker_count {
                        let next = &next;
                        let pids = &pids_to_init;
                        let initialized = &initialized;
                        scope.spawn(|| {
                            loop {
                                let idx = next.fetch_add(1, Ordering::Relaxed);
                                if idx >= pids.len() {
                                    break;
                                }
                                let pid = pids[idx];
                                match self.init_process(pid) {
                                    Ok(state) => initialized.lock().unwrap().push((pid, state)),
                                    Err(e) => tracing::warn!("Process {pid} init error: {e}"),
                                }
                            }
                        });
                    }
                });

                for (pid, state) in initialized.into_inner().unwrap() {
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
                if let Ok(beatmap_addr) = crate::beatmap::read_beatmap_ptr(&client.memory, base_addr) {
                    if beatmap_addr == 0 {
                        client.cached_beatmap_ptr = 0;
                        client.cached_beatmap_snapshot = None;
                        None
                    } else {
                        let live_time = crate::beatmap::read_live_time(&client.memory, client.play_time_pattern_addr);
                        if beatmap_addr == client.cached_beatmap_ptr && client.cached_beatmap_snapshot.is_some() {
                            let mut bm = client.cached_beatmap_snapshot.clone().unwrap();
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
                }
                if beatmap_ref.checksum != self.current_checksum
                    && !beatmap_ref.checksum.is_empty()
                {
                    self.current_checksum = beatmap_ref.checksum.clone();
                    self.cached_stats_by_mods.clear();
                    if let Some(beatmap_mut) = beatmap.as_mut() {
                        crate::beatmap::populate_beatmap_file_metadata(beatmap_mut, &osu_path);
                        self.cached_metadata = Some(beatmap_mut.clone());
                    }
                    #[cfg(feature = "pp")]
                    if let Ok(bytes) = std::fs::read(&osu_path) {
                        if let Ok(map) = rosu_pp::Beatmap::from_bytes(&bytes) {
                            let mods_legacy = crate::pp::calculator::parse_mods_bits(0);
                            let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(&map);
                            let mut temp_snap = beatmap.as_ref().cloned().unwrap_or_default();
                            crate::beatmap::populate_beatmap_statistics_with_diff(&mut temp_snap, &map, &diff, 0);
                            temp_snap.stats.stars.live = 0.0;
                            self.cached_stats = temp_snap.stats;
                            self.cached_accuracy = crate::pp::calculator::calc_accuracy_table_from_diff(&diff);
                            let first_obj = beatmap.as_ref().map_or(temp_snap.time.first_object, |b| {
                                if b.time.first_object > 0 { b.time.first_object } else { temp_snap.time.first_object }
                            });
                            let last_obj = beatmap.as_ref().map_or(temp_snap.time.last_object, |b| {
                                if b.time.last_object > 0 { b.time.last_object } else { temp_snap.time.last_object }
                            });
                            let mp3_len = beatmap.as_ref().map_or(temp_snap.time.mp3_length, |b| {
                                if b.time.mp3_length > 0 { b.time.mp3_length } else { temp_snap.time.mp3_length }
                            });
                            self.cached_graph = performance_graph(
                                &map,
                                0,
                                first_obj,
                                last_obj,
                                mp3_len,
                            );
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
            let need_user = client.ipc_id.is_some() && client.spectating_user_pattern_addr.is_none();
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
                    if let Ok((user_pat_src, user_pat_off)) = self.profile.pattern("spectating_user_ptr") {
                        if let Ok(user_pat) = BytePattern::parse(user_pat_src) {
                            if let Ok(matches) = client.memory.scan_pattern(&user_pat, None, 1, self.scan_limit_bytes) {
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
                            let cached = if client.cached_chat_size > 0 {
                                Some((client.cached_chat_size, client.cached_chat.as_slice()))
                            } else {
                                None
                            };
                            if let Ok(chat) =
                                read_tournament_chat(&client.memory, chat_pat, &spectator_teams, cached)
                            {
                                client.cached_chat_size = chat.len();
                                client.cached_chat = chat.clone();
                                tourney.chat = chat;
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
                        if user_addr != 0 && user_addr == client.cached_user_ptr && client.cached_user.is_some() {
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
                    crate::client::read_gameplay_state_cached(&client.memory, ruleset, cached).ok()
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
                        let diff_mods = g.mods & !(1 << 29); // Strip ScoreV2
                        if diff_mods == 0 {
                            beatmap.stats = self.cached_stats.clone();
                        } else if let Some(stats) = self.cached_stats_by_mods.get(&diff_mods) {
                            beatmap.stats = stats.clone();
                        } else {
                            let mut temp = self.cached_metadata.clone().unwrap_or_else(|| beatmap.clone());
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
                        let mods_legacy = gameplay
                            .as_ref()
                            .map(|g| crate::pp::calculator::parse_mods_bits(g.mods))
                            .unwrap_or_else(|| rosu_mods::GameModsLegacy::default());
                        let total_objects = map.hit_objects.len();
                        let chunks = crate::pp::calculator::get_or_compute_gradual_chunks(
                            beatmap.as_ref().map_or(0, |b| b.id as u32),
                            map,
                            mods_legacy,
                            self.gradual_pp_chunks,
                        );
                        let (combo, n300, n100, n50, n0) = gameplay
                            .as_ref()
                            .map(|g| (g.combo as u32, g.hit_300 as u32, g.hit_100 as u32, g.hit_50 as u32, g.hit_miss as u32))
                            .unwrap_or((0, 0, 0, 0, 0));
                        let pp = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                            &chunks,
                            total_objects,
                            mods_legacy,
                            combo,
                            n300,
                            n100,
                            n50,
                            n0,
                        );
                        let total_hits = n300 + n100 + n50 + n0;
                        let live_stars = crate::pp::calculator::live_stars_from_chunks(
                            &chunks,
                            total_objects,
                            total_hits,
                        );
                        if let Some(b) = beatmap.as_mut() {
                            b.stats.stars.live = live_stars;
                        }
                        Some(pp)
                    } else {
                        None
                    }
                } else {
                    None
                };
                #[cfg(not(feature = "pp"))]
                let live_pp: Option<crate::pp::LivePpResult> = None;

                let is_playing = gameplay.as_ref().map_or(false, |g| g.combo > 0 || (g.hit_300 + g.hit_100 + g.hit_50 + g.hit_miss) > 0);
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
        let manager_status = self.clients
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
                        b.is_break = map
                            .breaks
                            .iter()
                            .any(|b_break| live_f >= b_break.start_time && live_f <= b_break.end_time);
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

    fn init_process(&self, pid: u32) -> Result<CachedClientState> {
        let memory = ProcessMemory::open_with_pointer_size(pid, self.pointer_width)?;
        let command_line = memory.command_line().unwrap_or_default();
        let spectate_info = parse_spectate_client_arg(&command_line);
        let ipc_id = spectate_info.map(|(id, _)| id);
        let is_spectator = ipc_id.is_some();
        let is_manager = is_tournament_manager_cmd(&command_line)
            || (!is_spectator && (command_line.contains("-go") || command_line.contains("/go")));

        // Scan rulesets_addr pattern to find container pointer address
        let ruleset_container_addr = crate::client::resolve_ruleset_container(
            &memory,
            &self.profile,
            self.scan_limit_bytes,
        )
        .ok();

        // If spectator, scan spectating_user_ptr
        let mut spectating_user_pattern_addr = None;
        if is_spectator || !is_manager {
            if let Ok((user_pat_src, user_pat_off)) = self.profile.pattern("spectating_user_ptr") {
                if let Ok(user_pat) = BytePattern::parse(user_pat_src) {
                    if let Ok(matches) =
                        memory.scan_pattern(&user_pat, None, 1, self.scan_limit_bytes)
                    {
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
            if let Ok((chat_pat_src, chat_pat_off)) = self.profile.pattern("tournament_chat_engine")
            {
                if let Ok(chat_pat) = BytePattern::parse(chat_pat_src) {
                    if let Ok(matches) =
                        memory.scan_pattern(&chat_pat, None, 1, self.scan_limit_bytes)
                    {
                        if let Some(&first) = matches.first() {
                            if let Ok(addr) = checked_add_signed(first, chat_pat_off) {
                                chat_engine_pattern_addr = Some(addr);
                            }
                        }
                    }
                }
            }
        }
        let base_pattern_addr =
            self.profile
                .pattern("base_addr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let play_time_pattern_addr =
            self.profile
                .pattern("play_time_addr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let audio_length_pattern_addr =
            self.profile
                .pattern("get_audio_length_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let game_time_pattern_addr =
            self.profile
                .pattern("game_time_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let skin_pattern_addr =
            self.profile
                .pattern("skin_data_addr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let user_profile_pattern_addr =
            self.profile
                .pattern("user_profile_ptr")
                .ok()
                .and_then(|(source, offset)| {
                    find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
                });
        let raw_login_status_pattern_addr = self
            .profile
            .pattern("raw_login_status_ptr")
            .ok()
            .and_then(|(source, offset)| {
                find_pattern(&memory, source, offset, self.scan_limit_bytes).ok()
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
            cached_metadata: None,
            cached_total_hits: 0,
            cached_hit_errors: Arc::default(),
            cached_unstable_rate: 0.0,
            cached_gameplay: None,
            cached_beatmap_ptr: 0,
            cached_beatmap_snapshot: None,
            cached_user_ptr: 0,
            cached_user: None,
            cached_chat_size: 0,
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
    let clock_rate: f64 = if (mods & 64) != 0 || (mods & 512) != 0 {
        1.5
    } else if (mods & 256) != 0 {
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
        map.hit_objects.last().map_or(first_obj_time, |o| o.start_time)
    };
    let total_time = mp3_time / clock_rate;

    let empty_offset_l = (start / 400.0).floor().max(0.0) as usize;

    let mods_legacy = crate::pp::calculator::parse_mods_bits(mods);
    let strains = rosu_pp::Difficulty::new().mods(mods_legacy).strains(map);

    let mut aim = Vec::new();
    let mut aim_no_sliders = Vec::new();
    let mut speed = Vec::new();
    let mut flashlight = Vec::new();
    let has_flashlight_mod = (mods & 1024) != 0;

    let mut strain_count = 0;
    if let rosu_pp::any::Strains::Osu(values) = strains {
        strain_count = values.aim.len();
        aim = values.aim;
        aim_no_sliders = values.aim_no_sliders;
        speed = values.speed;
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
                name: "reading".to_string(),
                data: pad_series(vec![0.0; strain_count]),
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
    match value {
        0 => "osu",
        1 => "taiko",
        2 => "fruits",
        3 => "mania",
        _ => "",
    }
}

fn profile_state_from_local(profile: &LocalProfile) -> crate::v2::ProfileState {
    crate::v2::ProfileState {
        user_status: crate::v2::OsuStatusState {
            number: profile.raw_login_status,
            name: match profile.raw_login_status {
                0 => "reconnecting",
                256 => "guest",
                257 => "recieving_data",
                65537 => "disconnected",
                65793 => "connected",
                _ => "",
            }
            .to_string(),
        },
        bancho_status: crate::v2::OsuStatusState {
            number: profile.raw_bancho_status,
            name: match profile.raw_bancho_status {
                0 => "idle",
                1 => "afk",
                2 => "playing",
                3 => "editing",
                4 => "modding",
                5 => "multiplayer",
                6 => "watching",
                7 => "unknown",
                8 => "testing",
                9 => "submitting",
                10 => "paused",
                11 => "lobby",
                12 => "multiplaying",
                13 => "osuDirect",
                _ => "",
            }
            .to_string(),
        },
        id: profile.id,
        name: profile.name.clone(),
        mode: crate::v2::OsuStatusState {
            number: profile.play_mode,
            name: match profile.play_mode {
                0 => "osu",
                1 => "taiko",
                2 => "fruits",
                3 => "mania",
                _ => "",
            }
            .to_string(),
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
    const CODES: &str = "oc eu ad ae af ag ai al am an ao aq ar as at au aw az ba bb bd be bf bg bh bi bj bm bn bo br bs bt bv bw by bz ca cc cd cf cg ch ci ck cl cm cn co cr cu cv cw cx cy cz de dj dk dm do dz ec ee eg eh er es et fi fj fk fm fo fr fx ga gb gd ge gf gh gi gl gm gn gq gr gs gt gu gw gy hk hm hn hr ht hu id ie il in io iq ir is it jm jo jp ke kg kh ki km kn kp kr kw ky kz la lb lc li lk lr ls lt lu lv ly ma mc md mg mh mk ml mm mn mo mq mr ms mt mu mv mw mx my mz na nc ne nf ng ni nl no np nr nu nz om pa pe pf pg ph pk pl pm pn pr ps pt pw py qa re ro ru rw sa sb sc sd se sg sh si sj sk sl sm sn so sr st sv sy sz tc td tf tg th tj tk tm tn to tl tr tt tv tw tz ua ug um us uy uz va vc ve vg vi vn vu wf ws ye yt rs za zm me zw xx a2 o1 ax gg im je bl mf";
    if value < 1 {
        return "";
    }
    CODES
        .split_whitespace()
        .nth((value - 1) as usize)
        .unwrap_or("")
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
    cached_beatmap_metadata: crate::beatmap::BeatmapSnapshot,
    #[cfg(feature = "pp")]
    cached_stats: crate::beatmap::BeatmapStats,
    cached_beatmap_ptr: u64,
    last_skin_read: Instant,
    last_profile_read: Instant,
    last_scan_attempt: Instant,
    pub cached_packet: crate::v2::TosuV2Packet,
    pub enable_pp: bool,
    pub gradual_pp_chunks: usize,
    pub enable_hit_errors: bool,
    cached_hit_errors_total_hits: u32,
    cached_hit_errors: Arc<[i16]>,
    cached_unstable_rate: f64,
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
            cached_beatmap_metadata: crate::beatmap::BeatmapSnapshot::default(),
            #[cfg(feature = "pp")]
            cached_stats: crate::beatmap::BeatmapStats::default(),
            cached_beatmap_ptr: 0,
            last_skin_read: Instant::now() - Duration::from_secs(10),
            last_profile_read: Instant::now() - Duration::from_secs(10),
            last_scan_attempt: Instant::now() - Duration::from_secs(10),
            cached_packet: crate::v2::TosuV2Packet {
                profile: guest_profile_state(),
                ..Default::default()
            },
            enable_pp: true,
            gradual_pp_chunks: 100,
            enable_hit_errors: true,
            cached_hit_errors_total_hits: 0,
            cached_hit_errors: Arc::default(),
            cached_unstable_rate: 0.0,
        })
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
                    self.cached_mods = u32::MAX;
                }
                self.cached_packet.client = "none".to_string();
                self.cached_packet.state.name = "notRunning".to_string();
                return Ok(self.cached_packet.clone());
            }
        }

        if need_proc_open {
            let procs = list_processes(Some("osu!.exe")).unwrap_or_default();
            if procs.is_empty() {
                self.pid = None;
                self.memory = None;
                self.cached_packet.client = "none".to_string();
                self.cached_packet.state.name = "notRunning".to_string();
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

        let memory = self.memory.as_ref().unwrap();
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
            self.cached_packet.game.paused = state == 7;
        } else {
            self.cached_packet.game.focused = memory.is_foreground();
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
            if let Some(mods_addr) = self.menu_mods_pattern_addr {
                if let Ok(mods_ptr) = memory.read_indirect_pointer(mods_addr) {
                    if mods_ptr != 0 {
                        if let Ok(mods_val) = memory.read_u32(mods_ptr) {
                            self.cached_packet.play.mods = crate::v2::create_mods_state(
                                mods_val,
                                &crate::client::format_mods(mods_val),
                            );
                        }
                    }
                }
            }
        }

        // 3. Live audio playback time (updates continuously at 60 Hz)
        let live_time = crate::beatmap::read_live_time(memory, self.play_time_pattern_addr);
        self.cached_packet.beatmap.time.live = live_time;

        // 4. Hierarchical beatmap reading (pointer-gated)
        if let Some(base_addr) = self.base_pattern_addr {
            if let Ok(beatmap_addr) = crate::beatmap::read_beatmap_ptr(memory, base_addr) {
                if beatmap_addr == 0 {
                    if self.cached_beatmap_ptr != 0 {
                        self.cached_beatmap_ptr = 0;
                        self.cached_packet.beatmap = BeatmapSnapshot::default();
                    }
                } else {
                    let beatmap_ptr_changed = beatmap_addr != self.cached_beatmap_ptr;
                    let active_mods = self.cached_packet.play.mods.number;
                    #[cfg(feature = "pp")]
                    let mods_changed =
                        self.cached_mods != active_mods || self.cached_difficulty_attrs.is_none();

                    if beatmap_ptr_changed {
                        self.cached_beatmap_ptr = beatmap_addr;
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

                                let checksum_changed =
                                    bm.checksum != self.current_checksum && !bm.checksum.is_empty();
                                if checksum_changed {
                                    self.current_checksum = bm.checksum.clone();
                                    let osu_path = std::path::Path::new(&self.songs_folder)
                                        .join(&bm.folder)
                                        .join(&bm.filename);
                                    crate::beatmap::populate_beatmap_file_metadata(
                                        &mut bm, &osu_path,
                                    );
                                    self.cached_beatmap_metadata = bm.clone();

                                    #[cfg(feature = "pp")]
                                    {
                                        crate::instr_scope!(BeatmapFileRead);
                                        let file_bytes = std::fs::read(&osu_path);
                                        if let Ok(bytes) = &file_bytes {
                                            crate::instr_scope!(BeatmapParse);
                                            self.cached_beatmap =
                                                rosu_pp::Beatmap::from_bytes(bytes).ok();
                                        } else {
                                            self.cached_beatmap = None;
                                        }
                                        self.cached_difficulty_attrs = None;
                                        self.cached_mods = u32::MAX;
                                    }
                                } else if !self.current_checksum.is_empty() {
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
                                }

                                #[cfg(feature = "pp")]
                                if let Some(map) = &self.cached_beatmap {
                                    self.cached_mods = active_mods;
                                    let mods_legacy =
                                        crate::pp::calculator::parse_mods_bits(active_mods);
                                    crate::instr_scope!(PpDifficulty);
                                    let diff =
                                        rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);
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
                                        crate::pp::calculator::calc_accuracy_table_from_diff(&diff);
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
                                }

                                self.cached_packet.folders.game = self.game_folder.clone();
                                self.cached_packet.folders.songs = self.songs_folder.clone();
                                self.cached_packet.folders.beatmap = bm.folder.clone();
                                self.cached_packet.files.beatmap = bm.filename.clone();
                                self.cached_packet.files.background =
                                    bm.background_filename.clone();
                                self.cached_packet.files.audio = bm.audio_filename.clone();
                                self.cached_packet.direct_path.beatmap_folder = bm.folder.clone();
                                self.cached_packet.direct_path.beatmap_file =
                                    join_beatmap_path(&bm.folder, &bm.filename);
                                self.cached_packet.direct_path.beatmap_background =
                                    join_beatmap_path(&bm.folder, &bm.background_filename);
                                self.cached_packet.direct_path.beatmap_audio =
                                    join_beatmap_path(&bm.folder, &bm.audio_filename);

                                self.cached_packet.beatmap = bm;
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
            self.ruleset_container_addr =
                crate::client::resolve_ruleset_container(memory, &self.profile, self.scan_limit_bytes)
                    .ok();
        }

        let active_ruleset_addr = self.ruleset_container_addr
            .and_then(|addr| crate::client::read_active_ruleset(memory, addr));

        if current_state_num == 2 {
            if let Some(ruleset_addr) = active_ruleset_addr {
                let cached = Some((
                    self.cached_hit_errors_total_hits,
                    &self.cached_hit_errors,
                    self.cached_unstable_rate,
                ));
                if let Ok(mut g) =
                    crate::client::read_gameplay_state_cached(memory, ruleset_addr, cached)
                {
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
                    self.cached_packet.play.hits.slider_breaks = g.slider_breaks;
                    self.cached_packet.play.health_bar.normal = g.player_hp / 2.0;
                    self.cached_packet.play.health_bar.smooth = g.player_hp_smooth / 2.0;
                    self.cached_packet.play.hit_error_array = g.hit_error_array;
                    self.cached_packet.play.unstable_rate = g.unstable_rate;
                    self.cached_packet.play.rank.current = g.grade;
                    self.cached_packet.play.rank.max_this_play = g.grade_max;
                    if self.cached_packet.play.mods.number != g.mods {
                        self.cached_packet.play.mods =
                            crate::v2::create_mods_state(g.mods, &g.mods_str);
                    }

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
                                let total_objects = map.hit_objects.len();
                                let chunks = crate::pp::calculator::get_or_compute_gradual_chunks(
                                    self.cached_packet.beatmap.id as u32,
                                    map,
                                    mods_legacy,
                                    self.gradual_pp_chunks,
                                );
                                let live_pp = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                                    &chunks,
                                    total_objects,
                                    mods_legacy,
                                    g.combo as u32,
                                    g.hit_300 as u32,
                                    g.hit_100 as u32,
                                    g.hit_50 as u32,
                                    g.hit_miss as u32,
                                );
                                let live_stars = crate::pp::calculator::live_stars_from_chunks(
                                    &chunks,
                                    total_objects,
                                    total_hits,
                                );
                                self.cached_packet.beatmap.stats.stars.live = live_stars;
                                self.cached_live_pp = Some(live_pp);
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
                if let Ok(g) = crate::client::read_gameplay_state(memory, ruleset_addr) {
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
                    if self.cached_packet.play.mods.number != g.mods {
                        self.cached_packet.play.mods =
                            crate::v2::create_mods_state(g.mods, &g.mods_str);
                    }
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
                                let total_objects = map.hit_objects.len();
                                let chunks = crate::pp::calculator::get_or_compute_gradual_chunks(
                                    self.cached_packet.beatmap.id as u32,
                                    map,
                                    mods_legacy,
                                    self.gradual_pp_chunks,
                                );
                                let live_res = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                                    &chunks,
                                    total_objects,
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

        crate::instr_scope!(PacketClone);
        Ok(self.cached_packet.clone())
    }
}
