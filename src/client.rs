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
    pub mods: u32,
    pub mods_str: String,
    pub grade: String,
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
    if let Some(idx) = lower.find("-spectateclient").or_else(|| lower.find("/spectateclient")) {
        let after = &cmd[idx..];
        let mut tokens = after.split_whitespace();
        tokens.next(); // skip -spectateclient
        if let Some(id_str) = tokens.next() {
            if let Ok(id) = id_str.parse::<usize>() {
                let total = tokens.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
                return Some((id, total));
            }
        }
    }
    None
}

pub fn is_tournament_manager_cmd(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("-go") || lower.contains("/go") || lower.contains("tourney") || lower.contains("tournament")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModEntry {
    pub acronym: String,
}

pub fn format_mods(mods: u32) -> String {
    let mut res = String::new();
    if mods & 1 != 0 { res.push_str("NF"); }
    if mods & 2 != 0 { res.push_str("EZ"); }
    if mods & 4 != 0 { res.push_str("TD"); }
    if mods & 8 != 0 { res.push_str("HD"); }
    if mods & 16 != 0 { res.push_str("HR"); }
    if mods & 16384 != 0 {
        res.push_str("PF");
    } else if mods & 32 != 0 {
        res.push_str("SD");
    }
    if mods & 512 != 0 {
        res.push_str("NC");
    } else if mods & 64 != 0 {
        res.push_str("DT");
    }
    if mods & 128 != 0 { res.push_str("RX"); }
    if mods & 256 != 0 { res.push_str("HT"); }
    if mods & 1024 != 0 { res.push_str("FL"); }
    if mods & (1 << 22) != 0 {
        res.push_str("CN");
    } else if mods & 2048 != 0 {
        res.push_str("AT");
    }
    if mods & 4096 != 0 { res.push_str("SO"); }
    if mods & 8192 != 0 { res.push_str("AP"); }
    if mods & (1 << 15) != 0 { res.push_str("4K"); }
    if mods & (1 << 16) != 0 { res.push_str("5K"); }
    if mods & (1 << 17) != 0 { res.push_str("6K"); }
    if mods & (1 << 18) != 0 { res.push_str("7K"); }
    if mods & (1 << 19) != 0 { res.push_str("8K"); }
    if mods & (1 << 20) != 0 { res.push_str("FI"); }
    if mods & (1 << 21) != 0 { res.push_str("RD"); }
    if mods & (1 << 23) != 0 { res.push_str("TG"); }
    if mods & (1 << 24) != 0 { res.push_str("9K"); }
    if mods & (1 << 25) != 0 { res.push_str("10K"); }
    if mods & (1 << 26) != 0 { res.push_str("1K"); }
    if mods & (1 << 27) != 0 { res.push_str("3K"); }
    if mods & (1 << 28) != 0 { res.push_str("2K"); }
    if mods & (1 << 29) != 0 { res.push_str("V2"); }
    if mods & (1 << 30) != 0 { res.push_str("MR"); }
    res
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
        if has_hd_fl { "SSH".to_string() } else { "SS".to_string() }
    } else if r300 > 0.90 && r50 <= 0.01 && hit_miss == 0 {
        if has_hd_fl { "SH".to_string() } else { "S".to_string() }
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
        if id < cutoff { "left".to_string() } else { "right".to_string() }
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

    let total_hits = (hit_300 + hit_100 + hit_50 + hit_miss) as f64;
    let accuracy = if total_hits > 0.0 {
        ((hit_300 as f64 * 300.0 + hit_100 as f64 * 100.0 + hit_50 as f64 * 50.0)
            / (total_hits * 300.0))
            * 100.0
    } else {
        100.0
    };

    let has_hd_fl = (mods & 8 != 0) || (mods & 1024 != 0);
    let grade = calculate_grade(hit_300, hit_100, hit_50, hit_miss, 100.0, has_hd_fl);

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
        hit_geki,
        hit_katu,
        hit_miss,
        mods,
        mods_str,
        grade,
    })
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

    let has_hd_fl = (mods & 8 != 0) || (mods & 1024 != 0);
    let grade = calculate_grade(hit_300, hit_100, hit_50, hit_miss, player_hp, has_hd_fl);

    Ok(GameplayState {
        player_name: memory
            .read_dotnet_string_from_pointer(checked_add(score_base, 0x28)?, 256)
            .context("reading gameplay player name")?,
        mode: memory
            .read_i32(checked_add(score_base, 0x64)?)
            .context("reading gameplay mode")?,
        score,
        accuracy: memory
            .read_f64(checked_add(accuracy_base, 0x0c)?)
            .context("reading gameplay accuracy")?,
        player_hp,
        player_hp_smooth,
        combo,
        max_combo,
        hit_100,
        hit_300,
        hit_50,
        hit_geki,
        hit_katu,
        hit_miss,
        mods,
        mods_str,
        grade,
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
    use super::{GameplayState, ProcessSnapshotResult, format_mods, calculate_grade, parse_spectate_client_arg};

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
        assert_eq!(format_mods(536870912), "V2");
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
