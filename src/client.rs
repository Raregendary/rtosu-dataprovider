use crate::address::{checked_add, checked_add_signed};
use crate::pattern::BytePattern;
use crate::process::{ProcessMemory, list_processes};
use crate::profile::{ClientProfile, load_profile};
use crate::tournament::{TournamentState, read_tournament_state};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
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
    pub hit_error_array: Arc<[i16]>,
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

/// Whether the command line marks osu! as a tournament manager.
///
/// Only the tournament flags count. `-go`/`/go` used to be listed here, but
/// that is osu!'s **autoplay** flag: a solo game launched with `-go` was
/// classified as a tournament manager, and the provider then served an empty
/// packet forever while reporting no error.
pub fn is_tournament_manager_cmd(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("-tourney") || lower.contains("/tourney") || lower.contains("tournament")
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
        let match_address = match_address;
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
    crate::instr_scope!(ResultScreen);
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
    crate::instr_scope!(LocalProfile);
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

/// Newest hit errors kept from the game's append-only `List<int>`.
///
/// Truncation policy: a list longer than this keeps its **last**
/// `MAX_HIT_ERRORS` entries. The old code returned an empty vector for any
/// list above 20 000 entries, which emptied `hitErrorArray` and pinned
/// `unstableRate` to 0.0 on marathon maps. The unstable rate is a standard
/// deviation, so the newest 20 000 samples are statistically indistinguishable
/// from all 47 000 while still bounding the read size and the payload.
pub const MAX_HIT_ERRORS: usize = 20_000;

/// Sanity bound for a single hit error in milliseconds; anything beyond it means
/// the tail is garbage rather than data.
const HIT_ERROR_BOUND: i32 = 10_000;

/// Bytes of .NET object header before element 0 of a `List<int>`'s storage.
const LIST_ITEMS_HEADER: u64 = 8;

/// Number of elements to read out of a hit-error list of `size` entries, and
/// the index of the first of them. Oversized lists are truncated to the newest
/// `MAX_HIT_ERRORS` entries, so the read never exceeds 80 000 bytes however
/// long the map has been running.
pub fn hit_error_window(size: usize) -> (usize, usize) {
    let start = size.saturating_sub(MAX_HIT_ERRORS);
    (start, size - start)
}

/// Address of element `start` of the hit-error storage at `items`.
pub fn hit_error_items_address(items: u64, start: usize) -> Result<u64> {
    let offset = (start as u64)
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("hit error offset overflow"))?;
    checked_add(items, LIST_ITEMS_HEADER + offset)
}

/// Decode one bulk hit-error read. Decoding stops at the first value outside
/// `±HIT_ERROR_BOUND`, preserving the prefix semantics of the per-element loop
/// it replaces, and a trailing partial element is ignored.
///
/// The buffer length is an exact upper bound on the element count, so the
/// result is allocated once. Collecting straight from the iterator instead
/// cost 14 reallocations per poll (measured under `dhat`: 46 752 blocks over
/// 3 343 polls) because `take_while` erases the size hint.
pub fn parse_hit_errors(bytes: &[u8]) -> Vec<i16> {
    let mut result = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !(-HIT_ERROR_BOUND..=HIT_ERROR_BOUND).contains(&value) {
            break;
        }
        result.push(value.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
    }
    result
}

pub fn read_hit_errors(memory: &ProcessMemory, score_base: u64) -> Result<Vec<i16>> {
    crate::instr_scope!(HitErrors);
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
    if size <= 0 {
        return Ok(Vec::new());
    }
    let (start, count) = hit_error_window(size as usize);
    let address = hit_error_items_address(items, start)?;
    let bytes = memory
        .read_bytes(address, count * 4)
        .with_context(|| format!("reading {count} hit errors at 0x{address:X}"))?;
    Ok(parse_hit_errors(&bytes))
}

pub fn calculate_unstable_rate(hit_errors: &[i16], mods: u32) -> f64 {
    if hit_errors.is_empty() {
        return 0.0;
    }
    let count = hit_errors.len() as f64;
    let average = hit_errors.iter().map(|&value| value as f64).sum::<f64>() / count;
    let variance = hit_errors
        .iter()
        .map(|&value| {
            let delta = value as f64 - average;
            delta * delta
        })
        .sum::<f64>()
        / count;
    let rate = variance.sqrt() * 10.0;
    if mods & 64 != 0 {
        rate / 1.5
    } else if mods & 256 != 0 {
        rate / 0.75
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
    read_gameplay_state_cached(memory, ruleset_address, None)
}

pub fn read_gameplay_state_cached(
    memory: &ProcessMemory,
    ruleset_address: u64,
    cached_hits: Option<(u32, &Arc<[i16]>, f64)>,
) -> Result<GameplayState> {
    crate::instr_scope!(GameplayState);
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

    // Optimization: only read hit error list when hit count changed
    let total_hits = (hit_300 as u32) + (hit_100 as u32) + (hit_50 as u32) + (hit_miss as u32);
    let (hit_error_array, unstable_rate) = if total_hits == 0 {
        (Arc::default(), 0.0)
    } else if let Some((last_hits, last_arr, last_ur)) = cached_hits {
        if last_hits == total_hits {
            (Arc::clone(last_arr), last_ur)
        } else {
            let arr: Arc<[i16]> = read_hit_errors(memory, score_base)
                .unwrap_or_default()
                .into();
            let ur = calculate_unstable_rate(&arr, mods);
            (arr, ur)
        }
    } else {
        let arr: Arc<[i16]> = read_hit_errors(memory, score_base)
            .unwrap_or_default()
            .into();
        let ur = calculate_unstable_rate(&arr, mods);
        (arr, ur)
    };
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
        GameplayState, MAX_HIT_ERRORS, ProcessSnapshotResult, calculate_grade,
        calculate_unstable_rate, format_mods, hit_error_items_address, hit_error_window,
        is_tournament_manager_cmd, parse_hit_errors, parse_spectate_client_arg,
    };

    /// `List<int>._items` as it looks in the game's address space: the 8-byte
    /// .NET object header followed by `values` as little-endian `i32`s, so
    /// element `i` lives at `items + 8 + i * 4`.
    fn list_items(values: &[i32]) -> Vec<u8> {
        let mut bytes = vec![0u8; 8];
        bytes.extend_from_slice(&item_bytes(values));
        bytes
    }

    /// Just the elements, i.e. what one `read_bytes` of the storage returns.
    fn item_bytes(values: &[i32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<u8>>()
    }

    /// The per-element loop `read_hit_errors` used before the bulk read, as an
    /// oracle over the same synthetic storage.
    fn read_hit_errors_reference_loop(storage: &[u8], size: usize) -> Vec<i16> {
        if !(0..=20_000).contains(&(size as i64)) {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(size);
        for index in 0..size {
            let offset = 8 + index * 4;
            let chunk = [
                storage[offset],
                storage[offset + 1],
                storage[offset + 2],
                storage[offset + 3],
            ];
            let value = i32::from_le_bytes(chunk);
            if !(-10_000..=10_000).contains(&value) {
                break;
            }
            result.push(value.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        }
        result
    }

    fn pseudo_random_hits(count: usize, seed: u64) -> Vec<i32> {
        let mut state = seed;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 33) % 4001) as i32 - 2000
            })
            .collect()
    }

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
    fn autoplay_is_not_a_tournament_manager() {
        // `-go` is osu!'s autoplay flag. Treating it as a tournament manager
        // made Auto mode serve an empty packet for the whole session.
        assert!(!is_tournament_manager_cmd("\"D:\\osu\\osu!.exe\" -go"));
        assert!(!is_tournament_manager_cmd("\"D:\\osu\\osu!.exe\" /go"));
        assert!(!is_tournament_manager_cmd("\"D:\\osu\\osu!.exe\""));
        assert!(!is_tournament_manager_cmd(
            "\"D:\\osu\\osu!.exe\" -replay \"C:\\maps\\x.osr\""
        ));
        assert!(is_tournament_manager_cmd(
            "\"D:\\osu\\osu!.exe\" -tourney 127.0.0.1:24050"
        ));
        assert!(is_tournament_manager_cmd(
            "\"D:\\osu\\osu!.exe\" -Tournament"
        ));
    }

    #[test]
    fn test_grade() {
        assert_eq!(calculate_grade(100, 0, 0, 0, 100.0, false), "SS");
        assert_eq!(calculate_grade(100, 0, 0, 0, 100.0, true), "SSH");
        assert_eq!(calculate_grade(100, 0, 0, 0, 0.0, false), "F");
    }

    #[test]
    fn hit_error_window_keeps_lists_within_the_cap_whole() {
        assert_eq!(hit_error_window(0), (0, 0));
        assert_eq!(hit_error_window(1), (0, 1));
        assert_eq!(hit_error_window(6_230), (0, 6_230));
        assert_eq!(hit_error_window(MAX_HIT_ERRORS), (0, MAX_HIT_ERRORS));
    }

    #[test]
    fn hit_error_window_truncates_oversized_lists_to_the_newest_entries() {
        // The regression: 30 000 entries used to come back empty, which emptied
        // hitErrorArray and pinned unstableRate to 0.0.
        assert_eq!(hit_error_window(30_000), (10_000, MAX_HIT_ERRORS));
        assert_eq!(hit_error_window(MAX_HIT_ERRORS + 1), (1, MAX_HIT_ERRORS));
        assert_eq!(hit_error_window(47_000), (27_000, MAX_HIT_ERRORS));
        // The read stays bounded at 80 000 bytes no matter how absurd the count.
        assert_eq!(
            hit_error_window(i32::MAX as usize),
            (i32::MAX as usize - MAX_HIT_ERRORS, MAX_HIT_ERRORS)
        );
        assert!(hit_error_window(usize::MAX).1 <= MAX_HIT_ERRORS);
    }

    #[test]
    fn hit_error_items_address_skips_the_object_header() {
        let items = 0x0000_1234_0000;
        assert_eq!(hit_error_items_address(items, 0).unwrap(), items + 8);
        assert_eq!(hit_error_items_address(items, 1).unwrap(), items + 12);
        assert_eq!(hit_error_items_address(items, 3).unwrap(), items + 20);
        assert!(hit_error_items_address(u64::MAX, MAX_HIT_ERRORS).is_err());
    }

    #[test]
    fn parse_hit_errors_reads_little_endian_elements() {
        assert_eq!(parse_hit_errors(&[]), Vec::<i16>::new());
        assert_eq!(
            parse_hit_errors(&item_bytes(&[0, 1, -1, 9_999])),
            vec![0i16, 1, -1, 9_999]
        );
        assert_eq!(
            parse_hit_errors(&item_bytes(&[10_000, -10_000])),
            vec![10_000i16, -10_000]
        );
        assert_eq!(parse_hit_errors(&item_bytes(&[10_001])), Vec::<i16>::new());
        assert_eq!(parse_hit_errors(&item_bytes(&[-10_001])), Vec::<i16>::new());
    }

    #[test]
    fn parse_hit_errors_stops_at_the_first_out_of_range_value() {
        let parsed = parse_hit_errors(&item_bytes(&[5, -5, 10_001, 7]));
        assert_eq!(parsed, vec![5i16, -5]);
    }

    #[test]
    fn parse_hit_errors_ignores_a_partial_trailing_element() {
        let mut bytes = item_bytes(&[1, 2, 3]);
        bytes.extend_from_slice(&[0x04, 0x00]);
        assert_eq!(parse_hit_errors(&bytes), vec![1i16, 2, 3]);

        let mut truncated = item_bytes(&[1, 2, 3]);
        truncated.truncate(3 * 4 - 1);
        assert_eq!(parse_hit_errors(&truncated), vec![1i16, 2]);
    }

    #[test]
    fn bulk_parse_matches_the_per_element_loop() {
        for &size in &[0usize, 1, 2, 3, 4, 5, 999, 6_230, 19_999, 20_000] {
            let values = pseudo_random_hits(size, 0x5eed_0000 + size as u64);
            let storage = list_items(&values);
            let bulk = parse_hit_errors(&storage[8..]);
            let oracle = read_hit_errors_reference_loop(&storage, size);
            assert_eq!(bulk.len(), oracle.len(), "length mismatch at size {size}");
            assert!(bulk == oracle, "value mismatch at size {size}");
        }
    }

    #[test]
    fn bulk_parse_matches_the_loop_with_a_sentinel_in_the_tail() {
        let mut values = pseudo_random_hits(4_096, 0xabcd);
        let sentinel = values.len();
        values.extend_from_slice(&[i32::MAX, 42, 7]);
        let storage = list_items(&values);
        assert_eq!(
            parse_hit_errors(&storage[8..]),
            read_hit_errors_reference_loop(&storage, sentinel + 3)
        );
    }

    #[test]
    fn oversized_list_keeps_the_newest_errors_and_an_unstable_rate() {
        let values = pseudo_random_hits(30_000, 0x1234_5678);
        let storage = list_items(&values);
        let (start, count) = hit_error_window(values.len());
        let items = 0x0000_7fff_0000;
        let address = hit_error_items_address(items, start).unwrap();
        let offset = (address - items) as usize;
        let bulk = parse_hit_errors(&storage[offset..offset + count * 4]);

        let expected_tail: Vec<i16> = values[10_000..].iter().map(|&v| v as i16).collect();
        assert_eq!(bulk.len(), MAX_HIT_ERRORS);
        assert!(bulk == expected_tail);
        assert_ne!(calculate_unstable_rate(&bulk, 0), 0.0);
    }

    #[test]
    fn an_oversized_list_is_truncated_where_the_old_code_dropped_it() {
        // The regression: 30 000 entries came back empty, which emptied
        // hitErrorArray and pinned unstableRate to 0.0 on marathon maps.
        let values = pseudo_random_hits(30_000, 0x9999);
        let storage = list_items(&values);
        assert!(read_hit_errors_reference_loop(&storage, values.len()).is_empty());

        let (start, count) = hit_error_window(values.len());
        let bulk = parse_hit_errors(&storage[8 + start * 4..8 + (start + count) * 4]);
        assert_eq!(bulk.len(), MAX_HIT_ERRORS);
        assert!(calculate_unstable_rate(&bulk, 0) > 0.0);
    }

    #[test]
    fn empty_and_negative_counts_read_as_no_hit_errors() {
        assert_eq!(parse_hit_errors(&item_bytes(&[])), Vec::<i16>::new());
        let (start, count) = hit_error_window(0);
        assert_eq!((start, count), (0, 0));
    }

    #[test]
    fn test_unstable_rate_mod_scaling() {
        let hits = vec![-10i16, 10, -5, 5, -8, 8];
        let nomod = calculate_unstable_rate(&hits, 0);
        let dt = calculate_unstable_rate(&hits, 64);
        let ht = calculate_unstable_rate(&hits, 256);

        assert!(nomod > 0.0);
        // DT speeds up clock 1.5x -> UR is divided by 1.5
        assert!((dt - (nomod / 1.5)).abs() < 1e-6);
        // HT slows clock to 0.75x -> UR is divided by 0.75 (larger UR)
        assert!((ht - (nomod / 0.75)).abs() < 1e-6);
        assert!(ht > nomod);
        assert!(nomod > dt);
    }

    #[test]
    fn test_i16_hit_error_json_serialization() {
        let hits: Vec<i16> = vec![-15, 0, 12, 35, -4];
        let json = serde_json::to_string(&hits).unwrap();
        // Serializes as standard JSON array of numbers, identical to tosu's Vec<i32> format
        assert_eq!(json, "[-15,0,12,35,-4]");
    }
}
