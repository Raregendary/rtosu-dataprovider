use crate::address::checked_add_signed;
use crate::client::{
    is_tournament_manager_cmd, parse_spectate_client_arg, read_gameplay_state,
    read_tournament_user, GameplayState, TournamentUser,
};
use crate::pattern::BytePattern;
use crate::process::{ProcessMemory, list_processes};
use crate::profile::{ClientProfile, load_profile};
use crate::tournament::{
    read_tournament_chat, read_tournament_state, TournamentState,
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct TournamentClientView {
    pub pid: u32,
    pub ipc_id: usize,
    pub team: String,
    pub user: Option<TournamentUser>,
    pub gameplay: Option<GameplayState>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TournamentSnapshot {
    pub captured_at_ms: u128,
    pub poll_duration_us: u128,
    pub manager: Option<TournamentState>,
    pub clients: Vec<TournamentClientView>,
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
}

pub struct TournamentSession {
    profile: ClientProfile,
    pointer_width: Option<usize>,
    scan_limit_bytes: usize,
    clients: BTreeMap<u32, CachedClientState>,
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
        })
    }

    pub fn poll(&mut self) -> Result<TournamentSnapshot> {
        let start = Instant::now();

        // 1. Enumerate current osu processes
        let running_processes = list_processes(Some("osu!.exe"))?;
        let running_pids: HashMap<u32, String> = running_processes
            .into_iter()
            .map(|p| (p.pid, p.name))
            .collect();

        // 2. Remove dead processes from cache
        self.clients.retain(|pid, _| running_pids.contains_key(pid));

        // 3. Attach and initialize any newly discovered processes in parallel
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
                                Err(e) => eprintln!("Process {pid} init error: {e}"),
                            }
                        }
                    });
                }
            });

            for (pid, state) in initialized.into_inner().unwrap() {
                self.clients.insert(pid, state);
            }
        }

        // 4. Determine total spectators to set left/right team threshold
        let mut max_ipc_id = 0;
        let mut total_spectators = 0;
        for client in self.clients.values() {
            if let Some(ipc) = client.ipc_id {
                total_spectators += 1;
                if ipc > max_ipc_id {
                    max_ipc_id = ipc;
                }
            }
        }
        let spectator_count = if total_spectators > 0 {
            (max_ipc_id + 1).max(total_spectators)
        } else {
            6
        };
        let team_cutoff = spectator_count / 2;

        // 5. Read all clients and manager state with high-speed direct memory dereferences
        let mut manager_state: Option<TournamentState> = None;
        let mut spectator_views = Vec::new();
        let mut spectator_teams = HashMap::new();

        // Pre-pass: map spectator usernames to their assigned team for chat mapping
        for client in self.clients.values() {
            if let Some(ipc) = client.ipc_id {
                let team = if ipc < team_cutoff {
                    "left".to_string()
                } else {
                    "right".to_string()
                };
                if let Some(user_pat) = client.spectating_user_pattern_addr {
                    if let Ok(user_addr) = client.memory.read_indirect_pointer(user_pat) {
                        if let Ok(user) = read_tournament_user(&client.memory, user_addr) {
                            spectator_teams.insert(user.name, team);
                        }
                    }
                }
            }
        }

        for client in self.clients.values_mut() {
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

            let Some(ruleset_addr) = ruleset_addr else {
                continue;
            };

            // If it's a manager or potential manager
            if client.is_manager || client.ipc_id.is_none() {
                if let Ok(mut tourney) = read_tournament_state(&client.memory, ruleset_addr) {
                    client.is_manager = true;
                    if let Some(chat_pat) = client.chat_engine_pattern_addr {
                        if let Ok(chat) =
                            read_tournament_chat(&client.memory, chat_pat, &spectator_teams)
                        {
                            tourney.chat = chat;
                        }
                    }
                    manager_state = Some(tourney);
                    continue;
                }
            }

            // Spectator client
            if let Some(ipc_id) = client.ipc_id {
                let team = if ipc_id < team_cutoff {
                    "left".to_string()
                } else {
                    "right".to_string()
                };

                let user = match client.spectating_user_pattern_addr {
                    Some(pat_addr) => match client.memory.read_indirect_pointer(pat_addr) {
                        Ok(user_addr) => read_tournament_user(&client.memory, user_addr).ok(),
                        Err(_) => None,
                    },
                    None => None,
                };

                let gameplay = read_gameplay_state(&client.memory, ruleset_addr).ok();

                spectator_views.push(TournamentClientView {
                    pid: client.pid,
                    ipc_id,
                    team,
                    user,
                    gameplay,
                    error: None,
                });
            }
        }

        // Sort spectator views strictly by ipc_id
        spectator_views.sort_by_key(|c| c.ipc_id);

        let captured_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| anyhow::anyhow!("system clock is before Unix epoch: {error}"))?
            .as_millis();

        let poll_duration_us = start.elapsed().as_micros();

        Ok(TournamentSnapshot {
            captured_at_ms,
            poll_duration_us,
            manager: manager_state,
            clients: spectator_views,
        })
    }

    fn init_process(&self, pid: u32) -> Result<CachedClientState> {
        let memory = ProcessMemory::open_with_pointer_size(pid, self.pointer_width)?;
        let command_line = memory.command_line().unwrap_or_default();
        let spectate_info = parse_spectate_client_arg(&command_line);
        let ipc_id = spectate_info.map(|(id, _)| id);
        let is_spectator = ipc_id.is_some();
        let is_manager = is_tournament_manager_cmd(&command_line);

        // Scan rulesets_addr pattern to find container pointer address
        let (ruleset_pat_src, offset) = self.profile.pattern("rulesets_addr")?;
        let pattern = BytePattern::parse(ruleset_pat_src)?;
        let matches = memory.scan_pattern(&pattern, None, 16, self.scan_limit_bytes)?;
        let mut ruleset_container_addr = None;
        let mut fallback_container = None;
        for match_address in matches {
            let pattern_address = match checked_add_signed(match_address, offset) {
                Ok(address) => address,
                Err(_) => continue,
            };
            let container_addr = match pattern_address.checked_sub(0xb) {
                Some(address) => address,
                None => continue,
            };
            let container = match memory.read_pointer(container_addr) {
                Ok(addr) if addr != 0 => addr,
                _ => continue,
            };
            let ruleset = match memory.read_pointer(container.saturating_add(4)) {
                Ok(addr) if addr != 0 => addr,
                _ => continue,
            };
            let ipc_state = memory.read_i32(ruleset.saturating_add(0x54)).unwrap_or(0);
            if is_manager {
                if ipc_state > 0 || read_tournament_state(&memory, ruleset).is_ok() {
                    ruleset_container_addr = Some(container_addr);
                    break;
                }
            } else {
                if ipc_state & 2 == 0 {
                    let gp = memory.read_pointer(ruleset.saturating_add(0x64)).unwrap_or(0);
                    if gp != 0 || read_gameplay_state(&memory, ruleset).is_ok() {
                        ruleset_container_addr = Some(container_addr);
                        break;
                    }
                }
            }
            fallback_container.get_or_insert(container_addr);
        }
        let ruleset_container_addr = ruleset_container_addr.or(fallback_container);

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
            if let Ok((chat_pat_src, chat_pat_off)) =
                self.profile.pattern("tournament_chat_engine")
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
        })
    }
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
    menu_mods_pattern_addr: Option<u64>,
    ruleset_container_addr: Option<u64>,
    game_folder: String,
    songs_folder: String,
    current_checksum: String,
    #[cfg(feature = "pp")]
    cached_beatmap: Option<rosu_pp::Beatmap>,
    pub cached_packet: crate::v2::TosuV2Packet,
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
            menu_mods_pattern_addr: None,
            ruleset_container_addr: None,
            game_folder: String::new(),
            songs_folder: String::new(),
            current_checksum: String::new(),
            #[cfg(feature = "pp")]
            cached_beatmap: None,
            cached_packet: crate::v2::TosuV2Packet::default(),
        })
    }

    pub fn poll(&mut self) -> Result<crate::v2::TosuV2Packet> {
        // 1. Ensure we have a valid open process
        let procs = list_processes(Some("osu!.exe"))?;
        if procs.is_empty() {
            self.pid = None;
            self.memory = None;
            self.cached_packet.client = "none".to_string();
            self.cached_packet.state.name = "notRunning".to_string();
            return Ok(self.cached_packet.clone());
        }

        let pid = procs[0].pid;
        if self.pid != Some(pid) || self.memory.is_none() {
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
            self.menu_mods_pattern_addr = None;
            self.ruleset_container_addr = None;
            self.current_checksum.clear();
            #[cfg(feature = "pp")]
            {
                self.cached_beatmap = None;
            }
        }

        let memory = self.memory.as_ref().unwrap();

        // 2. Scan status_ptr if not cached
        if self.status_pattern_addr.is_none() {
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
        if self.base_pattern_addr.is_none() {
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
        if self.play_time_pattern_addr.is_none() {
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

        // 5. Scan menu_mods_ptr if not cached
        if self.menu_mods_pattern_addr.is_none() {
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

        self.cached_packet.client = "stable".to_string();
        self.cached_packet.server = "ppy.sh".to_string();

        // 6. Read state
        let mut current_state_num = 0;
        if let Some(status_addr) = self.status_pattern_addr {
            if let Ok(status_ptr) = memory.read_pointer(status_addr) {
                if status_ptr != 0 {
                    if let Ok(val) = memory.read_i32(status_ptr) {
                        current_state_num = val;
                        self.cached_packet.state.number = val;
                        self.cached_packet.state.name = crate::v2::osu_state_name(val).to_string();
                    }
                }
            }
        }

        // 7. Read beatmap
        if let Some(base_addr) = self.base_pattern_addr {
            if let Ok(mut bm) = crate::beatmap::read_beatmap_memory(
                memory,
                base_addr,
                self.play_time_pattern_addr,
                self.pointer_width,
            ) {
                if bm.id > 0 || !bm.title.is_empty() {
                    // Update cached beatmap if checksum changed
                    if bm.checksum != self.current_checksum && !bm.checksum.is_empty() {
                        self.current_checksum = bm.checksum.clone();
                        #[cfg(feature = "pp")]
                        {
                            let osu_path = std::path::Path::new(&self.songs_folder)
                                .join(&bm.folder)
                                .join(&bm.filename);
                            if let Ok(bytes) = std::fs::read(&osu_path) {
                                if let Ok(map) = rosu_pp::Beatmap::from_bytes(&bytes) {
                                    self.cached_beatmap = Some(map);
                                }
                            }
                        }
                    }

                    // Populate parsed objects & SS PP
                    #[cfg(feature = "pp")]
                    if let Some(map) = &self.cached_beatmap {
                        let current_mods_num = self.cached_packet.play.mods.number;
                        let mods_legacy = crate::pp::calculator::parse_mods_bits(current_mods_num);
                        let diff = rosu_pp::Difficulty::new().mods(mods_legacy).calculate(map);

                        let mut circles = 0;
                        let mut sliders = 0;
                        let mut spinners = 0;
                        for obj in &map.hit_objects {
                            if obj.is_circle() {
                                circles += 1;
                            } else if obj.is_slider() {
                                sliders += 1;
                            } else if obj.is_spinner() {
                                spinners += 1;
                            }
                        }
                        bm.stats.objects.circles = circles;
                        bm.stats.objects.sliders = sliders;
                        bm.stats.objects.spinners = spinners;
                        bm.stats.objects.total = map.hit_objects.len() as i32;
                        bm.stats.max_combo = diff.max_combo() as i32;
                        let bpm = map.bpm() as f32;
                        bm.stats.bpm.common = bpm;
                        bm.stats.bpm.realtime = bpm;
                        bm.stats.bpm.min = bpm;
                        bm.stats.bpm.max = bpm;
                        bm.stats.stars.total = diff.stars() as f32;
                        if let rosu_pp::any::DifficultyAttributes::Osu(osu_diff) = &diff {
                            bm.stats.stars.aim = osu_diff.aim as f32;
                            bm.stats.stars.speed = osu_diff.speed as f32;
                            bm.stats.stars.slider_factor = osu_diff.slider_factor as f32;
                        }
                        let ss_pp = crate::pp::calculator::calc_fc_pp(&diff, mods_legacy);
                        bm.stats.pp.ss = ss_pp;
                        bm.stats.pp.fc = ss_pp;
                    }

                    // Folders and files
                    self.cached_packet.folders.game = self.game_folder.clone();
                    self.cached_packet.folders.songs = self.songs_folder.clone();
                    self.cached_packet.folders.beatmap = bm.folder.clone();
                    self.cached_packet.files.beatmap = bm.filename.clone();
                    self.cached_packet.files.background = bm.background_filename.clone();
                    self.cached_packet.files.audio = bm.audio_filename.clone();
                    self.cached_packet.direct_path.beatmap_folder = bm.folder.clone();
                    self.cached_packet.direct_path.beatmap_file = format!("{}\\{}", bm.folder, bm.filename);
                    self.cached_packet.direct_path.beatmap_background = format!("{}\\{}", bm.folder, bm.background_filename);
                    self.cached_packet.direct_path.beatmap_audio = format!("{}\\{}", bm.folder, bm.audio_filename);

                    self.cached_packet.beatmap = bm;
                }
            }
        }

        // 8. Read gameplay or resultsScreen based on state
        if current_state_num == 2 {
            if self.ruleset_container_addr.is_none() {
                self.ruleset_container_addr = crate::client::resolve_ruleset(
                    memory,
                    &self.profile,
                    self.scan_limit_bytes,
                ).ok();
            }

            if let Some(ruleset_addr) = self.ruleset_container_addr {
                if let Ok(g) = crate::client::read_gameplay_state(memory, ruleset_addr) {
                    self.cached_packet.play.player_name = g.player_name;
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
                    self.cached_packet.play.health_bar.normal = g.player_hp;
                    self.cached_packet.play.health_bar.smooth = g.player_hp_smooth;
                    self.cached_packet.play.rank.current = g.grade;
                    self.cached_packet.play.mods = crate::v2::create_mods_state(g.mods, &g.mods_str);

                    #[cfg(feature = "pp")]
                    if let Some(map) = &self.cached_beatmap {
                        let mods_legacy = crate::pp::calculator::parse_mods_bits(g.mods);
                        let chunks = crate::pp::calculator::get_or_compute_gradual_chunks(
                            self.cached_packet.beatmap.id as u32,
                            map,
                            mods_legacy,
                        );
                        let live_res = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                            &chunks,
                            mods_legacy,
                            g.combo as u32,
                            g.hit_300 as u32,
                            g.hit_100 as u32,
                            g.hit_50 as u32,
                            g.hit_miss as u32,
                        );
                        self.cached_packet.play.pp = live_res;
                    }
                }
            }
        } else if current_state_num == 7 {
            // resultScreen
            if self.ruleset_container_addr.is_none() {
                self.ruleset_container_addr = crate::client::resolve_ruleset(
                    memory,
                    &self.profile,
                    self.scan_limit_bytes,
                ).ok();
            }

            if let Some(ruleset_addr) = self.ruleset_container_addr {
                if let Ok(res) = crate::client::read_result_screen_state(memory, ruleset_addr) {
                    let mods_state = crate::v2::create_mods_state(res.mods, &res.mods_str);
                    self.cached_packet.results_screen.score_id = res.online_id;
                    self.cached_packet.results_screen.player_name = res.player_name.clone();
                    self.cached_packet.results_screen.name = res.player_name.clone();
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

                    // Mirror to play
                    self.cached_packet.play.player_name = res.player_name;
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
                    self.cached_packet.play.rank.current = res.grade;
                    self.cached_packet.play.mods = mods_state;

                    #[cfg(feature = "pp")]
                    if let Some(map) = &self.cached_beatmap {
                        let mods_legacy = crate::pp::calculator::parse_mods_bits(res.mods);
                        let chunks = crate::pp::calculator::get_or_compute_gradual_chunks(
                            self.cached_packet.beatmap.id as u32,
                            map,
                            mods_legacy,
                        );
                        let live_res = crate::pp::calculator::calc_detailed_live_and_fc_pp(
                            &chunks,
                            mods_legacy,
                            res.max_combo as u32,
                            res.hit_300 as u32,
                            res.hit_100 as u32,
                            res.hit_50 as u32,
                            res.hit_miss as u32,
                        );
                        self.cached_packet.results_screen.pp.current = live_res.current;
                        self.cached_packet.results_screen.pp.fc = live_res.fc;
                        self.cached_packet.play.pp = live_res;
                    }
                }
            }
        } else {
            // In song select or menu, read menu mods
            if let Some(mods_addr) = self.menu_mods_pattern_addr {
                if let Ok(mods_ptr) = memory.read_pointer(mods_addr) {
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

        Ok(self.cached_packet.clone())
    }
}
