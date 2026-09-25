use crate::address::{checked_add, checked_add_signed};
use crate::pattern::BytePattern;
use crate::process::{ProcessMemory, list_processes};
use crate::profile::{ClientProfile, load_profile};
use crate::tournament::{TournamentState, read_tournament_state};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct TournamentUser {
    pub id: i32,
    pub name: String,
    pub country: String,
    pub accuracy: f64,
    pub ranked_score: i64,
    pub play_count: i32,
    pub global_rank: i32,
    pub pp: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalProfile {
    pub id: i32,
    pub name: String,
    pub accuracy: f64,
    pub ranked_score: i64,
    pub level: f32,
    pub play_count: i32,
    pub play_mode: i32,
    pub rank: i32,
    pub country_code: i32,
    pub performance_points: i32,
    pub raw_bancho_status: i32,
    pub raw_login_status: i32,
    pub background_colour: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameplayState {
    pub player_name: String,
    pub mode: i32,
    pub score: i32,
    pub accuracy: f64,
    pub player_hp: f64,
    pub player_hp_smooth: f64,
    pub combo: i16,
    pub max_combo: i16,
    pub hit_100: i16,
    pub hit_300: i16,
    pub hit_50: i16,
    pub hit_geki: i16,
    pub hit_katu: i16,
    pub hit_miss: i16,
    pub hit_error_array: Vec<i32>,
    pub slider_breaks: i32,
    pub mods: u32,
    pub mods_str: String,
    pub grade: String,
    pub grade_max: String,
    pub unstable_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultScreenState {
    pub online_id: i64,
    pub player_name: String,
    pub mode: i32,
    pub score: i32,
    pub accuracy: f64,
    pub max_combo: i16,
    pub hit_100: i16,
    pub hit_300: i16,
    pub hit_50: i16,
    pub hit_geki: i16,
    pub hit_katu: i16,
    pub hit_miss: i16,
    pub mods: u32,
    pub mods_str: String,
    pub grade: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientSnapshot {
    pub pid: u32,
    pub ipc_id: Option<usize>,
    pub team: Option<String>,
    pub profile: String,
    pub ruleset_address: u64,
    pub captured_at_ms: u128,
    pub user: Option<TournamentUser>,
    pub gameplay: Option<GameplayState>,
    pub tournament: Option<TournamentState>,
    pub errors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSnapshotResult {
    pub pid: u32,
    pub process_name: String,
    pub snapshot: Option<ClientSnapshot>,
    pub error: Option<String>,
}

pub fn parse_spectate_client_arg(cmd: &str) -> Option<(usize, usize)> {
    let lower = cmd.to_ascii_lowercase();
    if let Some(idx) = lower
        .find("-spectateclient")
        .or_else(|| lower.find("/spectateclient"))
    {
        let after = &cmd[idx..];
        let mut tokens = after.split_whitespace();
        tokens.next(); // skip -spectateclient
        if let Some(id_str) = tokens.next() {
            if let Ok(id) = id_str.parse::<usize>() {
                let total = tokens
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                return Some((id, total));
            }
        }
    }
    None
}

pub fn is_tournament_manager_cmd(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("-go")
        || lower.contains("/go")
        || lower.contains("tourney")
        || lower.contains("tournament")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModEntry {
    pub acronym: String,
}

pub fn format_mods(mods: u32) -> String {
    const VALUES: [(u32, &str, u8); 31] = [
        (1, "NF", 0),
        (2, "EZ", 1),
        (4, "TD", 7),
        (8, "HD", 2),
        (16, "HR", 4),
        (32, "SD", 5),
        (64, "DT", 3),
        (128, "RX", 99),
        (256, "HT", 3),
        (512, "NC", 3),
        (1024, "FL", 6),
        (2048, "AT", 99),
        (4096, "SO", 5),
        (8192, "AP", 99),
        (16384, "PF", 5),
        (1 << 15, "4K", 99),
        (1 << 16, "5K", 99),
        (1 << 17, "6K", 99),
        (1 << 18, "7K", 99),
        (1 << 19, "8K", 99),
        (1 << 20, "FI", 99),
        (1 << 21, "RD", 99),
        (1 << 22, "CN", 99),
        (1 << 23, "TG", 99),
        (1 << 24, "9K", 99),
        (1 << 25, "10K", 99),
        (1 << 26, "1K", 99),
        (1 << 27, "3K", 99),
        (1 << 28, "2K", 99),
        (1 << 29, "v2", 99),
        (1 << 30, "MR", 99),
    ];
    let mut parts: Vec<(u8, usize, &str)> = VALUES
        .iter()
        .enumerate()
        .filter_map(|(index, (bit, name, order))| {
            (mods & bit != 0).then_some((*order, index, *name))
        })
        .collect();
    parts.sort_by_key(|(order, index, _)| (*order, *index));
    parts
        .into_iter()
        .map(|(_, _, name)| name)
        .collect::<Vec<_>>()
        .join("")
        .replace("DTNC", "NC")
        .replace("SDPF", "PF")
        .replace("ATCN", "CN")
}

pub fn mod_acronyms(mods: u32) -> Vec<ModEntry> {
    let s = format_mods(mods);
    let mut entries = Vec::new();
    let mut chars = s.chars().peekable();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        entries.push(ModEntry {
            acronym: format!("{a}{b}"),
        });
    }
    entries
}

pub fn calculate_grade(
    hit_300: i16,
    hit_100: i16,
    hit_50: i16,
    hit_miss: i16,
    player_hp: f64,
    has_hd_fl: bool,
) -> String {
    if player_hp <= 0.0 {
        return "F".to_string();
    }
    let total = hit_300 as f64 + hit_100 as f64 + hit_50 as f64 + hit_miss as f64;
    if total <= 0.0 {
        return "SS".to_string();
    }
    let r300 = hit_300 as f64 / total;
    let r50 = hit_50 as f64 / total;
    if hit_300 as f64 == total {
        if has_hd_fl {
            "SSH".to_string()
        } else {
            "SS".to_string()
        }
    } else if r300 > 0.90 && r50 <= 0.01 && hit_miss == 0 {
        if has_hd_fl {
            "SH".to_string()
        } else {
            "S".to_string()
        }
    } else if (r300 > 0.80 && hit_miss == 0) || r300 > 0.90 {
        "A".to_string()
    } else if (r300 > 0.70 && hit_miss == 0) || r300 > 0.80 {
        "B".to_string()
    } else if r300 > 0.60 {
        "C".to_string()
    } else {
        "D".to_string()
    }
}

pub fn snapshot_process(
    pid: u32,
    profile_name: &str,
    pointer_width: Option<usize>,
    scan_limit_bytes: usize,
) -> Result<ClientSnapshot> {
    let mut last_error = None;
    for attempt in 0..3 {
        match snapshot_process_once(pid, profile_name, pointer_width, scan_limit_bytes) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => {
                last_error = Some(error);
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("snapshot failed")))
}

fn snapshot_process_once(
    pid: u32,
    profile_name: &str,
    pointer_width: Option<usize>,
    scan_limit_bytes: usize,
) -> Result<ClientSnapshot> {
    let profile = load_profile(profile_name)?;
    let width = pointer_width.or((profile.pointer_width > 0).then_some(profile.pointer_width));
    let memory = ProcessMemory::open_with_pointer_size(pid, width)?;
    let cmd = memory.command_line().unwrap_or_default();
    let spectate_info = parse_spectate_client_arg(&cmd);
    let ipc_id = spectate_info.map(|(id, _)| id);
    let team = spectate_info.map(|(id, total)| {
        let cutoff = if total > 0 { total / 2 } else { 3 };
        if id < cutoff {
            "left".to_string()
        } else {
            "right".to_string()
        }
    });

    let ruleset_address = resolve_ruleset(&memory, &profile, scan_limit_bytes)?;
    let mut errors = BTreeMap::new();
    let user = match (|| {
        let (user_pattern, user_pattern_offset) = profile.pattern("spectating_user_ptr")?;
        let user_pattern_address =
            find_pattern(&memory, user_pattern, user_pattern_offset, scan_limit_bytes)?;
        let user_address = memory
            .read_indirect_pointer(user_pattern_address)
            .context("reading spectating user address")?;
        read_tournament_user(&memory, user_address)
    })() {
        Ok(value) => Some(value),
        Err(error) => {
            errors.insert("user".to_owned(), error.to_string());
            None
        }
    };
    let gameplay = match read_gameplay_state(&memory, ruleset_address) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.insert("gameplay".to_owned(), error.to_string());
            None
        }
    };
    let tournament = match read_tournament_state(&memory, ruleset_address) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.insert("tournament".to_owned(), error.to_string());
            None
        }
    };
    if user.is_none() && gameplay.is_none() && tournament.is_none() {
        bail!("no readable client state: {errors:?}");
    }

    let captured_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| anyhow::anyhow!("system clock is before Unix epoch: {error}"))?
        .as_millis();
    Ok(ClientSnapshot {
        pid,
        ipc_id,
        team,
        profile: profile.id,
        ruleset_address,
        captured_at_ms,
        user,
        gameplay,
        tournament,
        errors,
    })
}

pub fn snapshot_processes(
    profile_name: &str,
    pointer_width: Option<usize>,
    scan_limit_bytes: usize,
) -> Result<Vec<ProcessSnapshotResult>> {
    let processes = list_processes(Some("osu!.exe"))?;
    let results = Mutex::new(Vec::with_capacity(processes.len()));
    let next = AtomicUsize::new(0);
    let worker_count = processes.len().clamp(1, 4);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let next = &next;
            let processes = &processes;
            let results = &results;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(process) = processes.get(index) else {
                        break;
                    };
                    let result = match snapshot_process(
                        process.pid,
                        profile_name,
                        pointer_width,
                        scan_limit_bytes,
                    ) {
                        Ok(snapshot) => ProcessSnapshotResult {
                            pid: process.pid,
                            process_name: process.name.clone(),
                            snapshot: Some(snapshot),
                            error: None,
                        },
                        Err(error) => ProcessSnapshotResult {
                            pid: process.pid,
                            process_name: process.name.clone(),
                            snapshot: None,
                            error: Some(error.to_string()),
                        },
                    };
                    results
                        .lock()
                        .expect("snapshot result lock poisoned")
                        .push(result);
                }
            });
        }
    });

    let mut results = results.into_inner().expect("snapshot result lock poisoned");
    // Sort by ipc_id if present, else by pid
    results.sort_by(|a, b| {
        let a_ipc = a.snapshot.as_ref().and_then(|s| s.ipc_id);
        let b_ipc = b.snapshot.as_ref().and_then(|s| s.ipc_id);
        match (a_ipc, b_ipc) {
            (Some(a_i), Some(b_i)) => a_i.cmp(&b_i),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.pid.cmp(&b.pid),
        }
    });
    Ok(results)
}

pub fn resolve_ruleset(
    memory: &ProcessMemory,
    profile: &ClientProfile,
    scan_limit_bytes: usize,
) -> Result<u64> {
    let (pattern_source, offset) = profile.pattern("rulesets_addr")?;
    let pattern = BytePattern::parse(pattern_source)?;
    let matches = memory.scan_pattern(&pattern, None, 16, scan_limit_bytes)?;
    let mut fallback = None;
    for match_address in matches {
        let pattern_address = match checked_add_signed(match_address, offset) {
            Ok(address) => address,
            Err(_) => continue,
        };
        let container_address = match pattern_address.checked_sub(0xb) {
            Some(address) => address,
            None => continue,
        };
        let container_address = match memory.read_pointer(container_address) {
            Ok(address) if address != 0 => address,
            _ => continue,
        };
        let ruleset_slot = match checked_add(container_address, 4) {
            Ok(address) => address,
            Err(_) => continue,
        };
        let candidate = match memory.read_pointer(ruleset_slot) {
            Ok(address) if address != 0 => address,
            _ => continue,
        };
        if read_gameplay_state(memory, candidate).is_ok()
            || read_tournament_state(memory, candidate).is_ok()
            || read_result_screen_state(memory, candidate).is_ok()
        {
            return Ok(candidate);
        }
        fallback.get_or_insert(candidate);
    }
    fallback.ok_or_else(|| anyhow::anyhow!("no valid ruleset candidate was found"))
}

pub fn calculate_accuracy(
    mode: i32,
    hit_300: i16,
    hit_100: i16,
    hit_50: i16,
    hit_miss: i16,
) -> f64 {
    match mode {
        0 => {
            let total = (hit_300 + hit_100 + hit_50 + hit_miss) as f64;
            if total == 0.0 {
                100.0
            } else {
                (hit_300 as f64 * 300.0 + hit_100 as f64 * 100.0 + hit_50 as f64 * 50.0)
                    / (total * 300.0)
                    * 100.0
            }
        }
        1 => {
            let total = (hit_300 + hit_100 + hit_miss) as f64;
            if total == 0.0 {
                100.0
            } else {
                (hit_300 as f64 * 1.0 + hit_100 as f64 * 0.5) / total * 100.0
            }
        }
        2 => {
            let total = (hit_300 + hit_100 + hit_50 + hit_miss) as f64;
            if total == 0.0 {
                100.0
            } else {
                (hit_300 as f64 * 300.0 + hit_100 as f64 * 100.0 + hit_50 as f64 * 50.0) / total
                    * 100.0
            }
        }
        3 => {
            let total = (hit_300 + hit_100 + hit_50 + hit_miss) as f64;
            if total == 0.0 {
                100.0
            } else {
                (hit_300 as f64 / total) * 100.0
            }
        }
        _ => 0.0,
    }
}

pub fn read_result_screen_state(
    memory: &ProcessMemory,
    ruleset_address: u64,
) -> Result<ResultScreenState> {
    let result_screen_base = memory
        .read_pointer(checked_add(ruleset_address, 0x38)?)
        .context("reading result screen base")?;
    if result_screen_base == 0 {
        bail!("resultScreenBase is null");
    }

    let online_id = memory
        .read_i64(checked_add(result_screen_base, 0x4)?)
        .unwrap_or(0);
    let player_name = memory
        .read_dotnet_string_from_pointer(checked_add(result_screen_base, 0x28)?, 256)
        .unwrap_or_default();

    let mods_ptr = memory
        .read_pointer(checked_add(result_screen_base, 0x1c)?)
        .unwrap_or(0);
    let mods = if mods_ptr != 0 {
        let x = memory.read_i32(checked_add(mods_ptr, 0xc)?).unwrap_or(0);
        let y = memory.read_i32(checked_add(mods_ptr, 0x8)?).unwrap_or(0);
        (x ^ y) as u32
    } else {
        0
    };
    let mods_str = format_mods(mods);

    let mode = memory
        .read_i32(checked_add(result_screen_base, 0x64)?)
        .unwrap_or(0);
    let max_combo = memory
        .read_i16(checked_add(result_screen_base, 0x68)?)
        .unwrap_or(0);
    let score = memory
        .read_i32(checked_add(result_screen_base, 0x78)?)
        .unwrap_or(0);

    let hit_100 = memory
        .read_i16(checked_add(result_screen_base, 0x88)?)
        .unwrap_or(0);
    let hit_300 = memory
        .read_i16(checked_add(result_screen_base, 0x8a)?)
        .unwrap_or(0);
    let hit_50 = memory
        .read_i16(checked_add(result_screen_base, 0x8c)?)
        .unwrap_or(0);
    let hit_geki = memory
        .read_i16(checked_add(result_screen_base, 0x8e)?)
        .unwrap_or(0);
    let hit_katu = memory
        .read_i16(checked_add(result_screen_base, 0x90)?)
        .unwrap_or(0);
    let hit_miss = memory
        .read_i16(checked_add(result_screen_base, 0x92)?)
        .unwrap_or(0);

    let accuracy = calculate_accuracy(mode, hit_300, hit_100, hit_50, hit_miss);
    let created_at = net_date_to_iso(memory, result_screen_base).unwrap_or_default();
    let grade = calculate_tosu_grade(mode, accuracy, hit_300, hit_100, hit_50, hit_miss, mods);

    Ok(ResultScreenState {
        online_id,
        player_name,
        mode,
        score,
        accuracy,
        max_combo,
        hit_100,
        hit_300,
        hit_50,
        hit_geki: if mode == 1 || mode == 3 { hit_geki } else { 0 },
        hit_katu: if mode == 1 || mode == 2 || mode == 3 {
            hit_katu
        } else {
            0
        },
        hit_miss,
        mods,
        mods_str,
        grade,
        created_at,
    })
}

pub fn read_local_profile(
    memory: &ProcessMemory,
    user_profile_pattern_addr: u64,
    raw_login_status_pattern_addr: u64,
) -> Result<LocalProfile> {
    let profile_base = memory
        .read_indirect_pointer(user_profile_pattern_addr)
        .context("reading local user profile pointer")?;
    if profile_base == 0 {
        bail!("local user profile is null");
    }
    let raw_login_status = if raw_login_status_pattern_addr == 0 {
        256
    } else {
        memory
            .read_indirect_pointer(raw_login_status_pattern_addr)
            .context("reading local login status")? as i32
    };
    Ok(LocalProfile {
        id: memory
            .read_i32(checked_add(profile_base, 0x70)?)
            .context("reading local user id")?,
        name: memory
            .read_dotnet_string_from_pointer(checked_add(profile_base, 0x30)?, 256)
            .context("reading local user name")?,
        accuracy: memory
            .read_f64(checked_add(profile_base, 0x04)?)
            .context("reading local user accuracy")?,
        ranked_score: memory
            .read_i64(checked_add(profile_base, 0x0c)?)
            .context("reading local ranked score")?,
        level: memory
            .read_f32(checked_add(profile_base, 0x74)?)
            .context("reading local user level")?,
        play_count: memory
            .read_i32(checked_add(profile_base, 0x7c)?)
            .context("reading local play count")?,
        play_mode: memory
            .read_i32(checked_add(profile_base, 0x80)?)
            .context("reading local play mode")?,
        rank: memory
            .read_i32(checked_add(profile_base, 0x84)?)
            .context("reading local global rank")?,
        country_code: memory
            .read_i32(checked_add(profile_base, 0x9c)?)
            .context("reading local country code")?,
        performance_points: memory
            .read_i32(checked_add(profile_base, 0x88)?)
            .context("reading local performance points")?,
        raw_bancho_status: memory
            .read_u8(checked_add(profile_base, 0x8c)?)
            .context("reading local bancho status")? as i32,
        raw_login_status,
        background_colour: memory
            .read_u32(checked_add(profile_base, 0xac)?)
            .context("reading local background colour")?,
    })
}

pub fn read_hit_errors(memory: &ProcessMemory, score_base: u64) -> Result<Vec<i32>> {
    let list = memory
        .read_pointer(checked_add(score_base, 0x38)?)
        .context("reading hit error list")?;
    if list == 0 {
        return Ok(Vec::new());
    }
    let items = memory
        .read_pointer(checked_add(list, 0x4)?)
        .context("reading hit error items")?;
    if items == 0 {
        return Ok(Vec::new());
    }
    let size = memory
        .read_i32(checked_add(list, 0xc)?)
        .context("reading hit error count")?;
    if !(0..=20_000).contains(&size) {
        return Ok(Vec::new());
    }
    let mut result = Vec::with_capacity(size as usize);
    for index in 0..size as u64 {
        let address = checked_add(items, 8 + index * 4)?;
        let value = memory.read_i32(address).unwrap_or(0);
        if !(-10_000..=10_000).contains(&value) {
            break;
        }
        result.push(value);
    }
    Ok(result)
}

pub fn calculate_unstable_rate(hit_errors: &[i32], mods: u32) -> f64 {
    if hit_errors.is_empty() {
        return 0.0;
    }
    let count = hit_errors.len() as f64;
    let average = hit_errors.iter().map(|value| *value as f64).sum::<f64>() / count;
    let variance = hit_errors
        .iter()
        .map(|value| {
            let delta = *value as f64 - average;
            delta * delta
        })
        .sum::<f64>()
        / count;
    let rate = variance.sqrt() * 10.0;
    if mods & 64 != 0 {
        rate / 1.5
    } else if mods & 256 != 0 {
        rate / 1.3333
    } else {
        rate
    }
}

pub fn calculate_tosu_grade(
    mode: i32,
    accuracy: f64,
    hit_300: i16,
    hit_100: i16,
    hit_50: i16,
    hit_miss: i16,
    mods: u32,
) -> String {
    let silver = mods & 8 != 0 || mods & 1024 != 0;
    let perfect = if silver { "XH" } else { "X" };
    let s_hit = if silver { "SH" } else { "S" };
    match mode {
        0 | 1 => {
            let total = hit_300 as f64 + hit_100 as f64 + hit_50 as f64 + hit_miss as f64;
            if total == 0.0 {
                return perfect.to_string();
            }
            let r300 = hit_300 as f64 / total;
            let r50 = hit_50 as f64 / total;
            if r300 == 1.0 {
                perfect.to_string()
            } else if r300 > 0.9 && r50 < 0.01 && hit_miss == 0 {
                s_hit.to_string()
            } else if (r300 > 0.8 && hit_miss == 0) || r300 > 0.9 {
                "A".to_string()
            } else if (r300 > 0.7 && hit_miss == 0) || r300 > 0.8 {
                "B".to_string()
            } else if r300 > 0.6 {
                "C".to_string()
            } else {
                "D".to_string()
            }
        }
        2 => {
            if accuracy >= 100.0 {
                perfect.to_string()
            } else if accuracy > 98.0 {
                s_hit.to_string()
            } else if accuracy > 94.0 {
                "A".to_string()
            } else if accuracy > 90.0 {
                "B".to_string()
            } else if accuracy > 85.0 {
                "C".to_string()
            } else {
                "D".to_string()
            }
        }
        3 => {
            if accuracy >= 100.0 {
                perfect.to_string()
            } else if accuracy >= 95.0 {
                s_hit.to_string()
            } else if accuracy >= 90.0 {
                "A".to_string()
            } else if accuracy >= 80.0 {
                "B".to_string()
            } else if accuracy >= 70.0 {
                "C".to_string()
            } else {
                "D".to_string()
            }
        }
        _ => String::new(),
    }
}

fn net_date_to_iso(memory: &ProcessMemory, base: u64) -> Result<String> {
    let low = memory.read_i32(checked_add(base, 0xa0)?)? as u32;
    let high = (memory.read_i32(checked_add(base, 0xa4)?)? as u32) & 0x3fff_ffff;
    let ticks = (high as u64) << 32 | low as u64;
    let epoch_ticks: u64 = 621_355_968_000_000_000;
    let milliseconds = ticks.saturating_sub(epoch_ticks) / 10_000;
    let seconds = (milliseconds / 1000) as i64;
    let fraction_millis = (milliseconds % 1000) as u32;
    let datetime = chrono_like_datetime(seconds, fraction_millis)?;
    Ok(datetime)
}

fn chrono_like_datetime(seconds: i64, fraction_millis: u32) -> Result<String> {
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fraction_millis:03}Z"
    ))
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

pub fn read_tournament_user(memory: &ProcessMemory, user_address: u64) -> Result<TournamentUser> {
    if user_address == 0 {
        bail!("user address is null");
    }
    Ok(TournamentUser {
        id: memory
            .read_i32(checked_add(user_address, 0x70)?)
            .context("reading tournament user id")?,
        name: memory
            .read_dotnet_string_from_pointer(checked_add(user_address, 0x30)?, 256)
            .context("reading tournament user name")?,
        country: memory
            .read_dotnet_string_from_pointer(checked_add(user_address, 0x2c)?, 128)
            .context("reading tournament user country")?,
        accuracy: memory
            .read_f64(checked_add(user_address, 0x04)?)
            .context("reading tournament user accuracy")?,
        ranked_score: memory
            .read_i64(checked_add(user_address, 0x0c)?)
            .context("reading tournament user ranked score")?,
        play_count: memory
            .read_i32(checked_add(user_address, 0x7c)?)
            .context("reading tournament user play count")?,
        global_rank: memory
            .read_i32(checked_add(user_address, 0x84)?)
            .context("reading tournament user global rank")?,
        pp: memory
            .read_i32(checked_add(user_address, 0x9c)?)
            .context("reading tournament user pp")?,
    })
}

pub fn read_gameplay_state(memory: &ProcessMemory, ruleset_address: u64) -> Result<GameplayState> {
    let gameplay_base = memory
        .read_pointer(checked_add(ruleset_address, 0x64)?)
        .context("reading gameplay base")?;
    if gameplay_base == 0 {
        bail!("gameplay base is null");
    }
    let score_base = memory
        .read_pointer(checked_add(gameplay_base, 0x38)?)
        .context("reading score base")?;
    let hp_bar_base = memory
        .read_pointer(checked_add(gameplay_base, 0x40)?)
        .context("reading health bar base")?;
    let accuracy_base = memory
        .read_pointer(checked_add(gameplay_base, 0x48)?)
        .context("reading accuracy base")?;
    if score_base == 0 || hp_bar_base == 0 || accuracy_base == 0 {
        bail!("gameplay child pointer is null");
    }
    let score_processor = memory
        .read_pointer(checked_add(score_base, 0x54)?)
        .context("reading score processor")?;
    let score = if score_processor == 0 {
        memory
            .read_i32(checked_add(score_base, 0x78)?)
            .context("reading score v1")?
    } else {
        memory
            .read_i32(checked_add(ruleset_address, 0xf8)?)
            .context("reading score v2")?
    };

    let mods_ptr = memory
        .read_pointer(checked_add(score_base, 0x1c)?)
        .unwrap_or(0);
    let mut mods = if mods_ptr != 0 {
        let x = memory.read_i32(checked_add(mods_ptr, 0xc)?).unwrap_or(0);
        let y = memory.read_i32(checked_add(mods_ptr, 0x8)?).unwrap_or(0);
        (x ^ y) as u32
    } else {
        0
    };
    if score_processor != 0 {
        mods |= 536870912; // ScoreV2 (1 << 29)
    }
    let mods_str = format_mods(mods);

    let player_hp = memory
        .read_f64(checked_add(hp_bar_base, 0x1c)?)
        .context("reading gameplay health")?;
    let player_hp_smooth = memory
        .read_f64(checked_add(hp_bar_base, 0x14)?)
        .context("reading gameplay smooth health")?;

    let combo = memory
        .read_i16(checked_add(score_base, 0x94)?)
        .context("reading gameplay combo")?;
    let max_combo = memory
        .read_i16(checked_add(score_base, 0x68)?)
        .context("reading gameplay maximum combo")?;
    let hit_100 = memory
        .read_i16(checked_add(score_base, 0x88)?)
        .context("reading 100 count")?;
    let hit_300 = memory
        .read_i16(checked_add(score_base, 0x8a)?)
        .context("reading 300 count")?;
    let hit_50 = memory
        .read_i16(checked_add(score_base, 0x8c)?)
        .context("reading 50 count")?;
    let hit_geki = memory
        .read_i16(checked_add(score_base, 0x8e)?)
        .context("reading geki count")?;
    let hit_katu = memory
        .read_i16(checked_add(score_base, 0x90)?)
        .context("reading katu count")?;
    let hit_miss = memory
        .read_i16(checked_add(score_base, 0x92)?)
        .context("reading miss count")?;
    let mode = memory
        .read_i32(checked_add(score_base, 0x64)?)
        .context("reading gameplay mode")?;
    let accuracy = memory
        .read_f64(checked_add(accuracy_base, 0x0c)?)
        .context("reading gameplay accuracy")?;
    let hit_error_array = read_hit_errors(memory, score_base).unwrap_or_default();
    let unstable_rate = calculate_unstable_rate(&hit_error_array, mods);
    let grade = calculate_tosu_grade(mode, accuracy, hit_300, hit_100, hit_50, hit_miss, mods);
    let grade_max = grade.clone();

    Ok(GameplayState {
        player_name: memory
            .read_dotnet_string_from_pointer(checked_add(score_base, 0x28)?, 256)
            .context("reading gameplay player name")?,
        mode,
        score,
        accuracy,
        player_hp,
        player_hp_smooth,
        combo,
        max_combo,
        hit_100,
        hit_300,
        hit_50,
        hit_geki: if mode == 1 || mode == 3 { hit_geki } else { 0 },
        hit_katu: if mode == 1 || mode == 2 || mode == 3 {
            hit_katu
        } else {
            0
        },
        hit_miss,
        hit_error_array,
        slider_breaks: 0,
        mods,
        mods_str,
        grade,
        grade_max,
        unstable_rate,
    })
}

pub fn find_pattern(
    memory: &ProcessMemory,
    pattern_source: &str,
    pattern_offset: i64,
    scan_limit_bytes: usize,
) -> Result<u64> {
    let pattern = BytePattern::parse(pattern_source)?;
    let matches = memory.scan_pattern(&pattern, None, 1, scan_limit_bytes)?;
    let match_address = matches
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("pattern '{pattern_source}' was not found"))?;
    checked_add_signed(match_address, pattern_offset)
}

#[cfg(test)]
mod tests {
    use super::{
        GameplayState, ProcessSnapshotResult, calculate_grade, format_mods,
        parse_spectate_client_arg,
    };

    #[test]
    fn snapshot_types_are_serializable() {
        let _ = std::mem::size_of::<GameplayState>();
        let _ = std::mem::size_of::<ProcessSnapshotResult>();
    }

    #[test]
    fn test_format_mods() {
        assert_eq!(format_mods(0), "");
        assert_eq!(format_mods(8), "HD");
        assert_eq!(format_mods(16), "HR");
        assert_eq!(format_mods(24), "HDHR");
        assert_eq!(format_mods(64), "DT");
        assert_eq!(format_mods(512), "NC");
        assert_eq!(format_mods(536870912), "v2");
    }

    #[test]
    fn test_parse_spectate_arg() {
        let cmd = "\"C:\\osu!.exe\" -spectateclient 2 6";
        assert_eq!(parse_spectate_client_arg(cmd), Some((2, 6)));

        let cmd_mgr = "\"C:\\osu!.exe\" -go";
        assert_eq!(parse_spectate_client_arg(cmd_mgr), None);
    }

    #[test]
    fn test_grade() {
        assert_eq!(calculate_grade(100, 0, 0, 0, 100.0, false), "SS");
        assert_eq!(calculate_grade(100, 0, 0, 0, 100.0, true), "SSH");
        assert_eq!(calculate_grade(100, 0, 0, 0, 0.0, false), "F");
    }
}
