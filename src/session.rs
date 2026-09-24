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
