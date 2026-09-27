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
    /// The four key-overlay buttons, read from osu! stable memory. Consumed by
    /// the precise payload, v1's `gameplay.keyOverlay` and SC's `keyOverlay`
    /// string -- three consumers of one read, so the walk happens here rather
    /// than three times.
    pub key_overlay: crate::v2::KeyOverlay,
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

/// Allocation-free tokenizer over a Windows command line.
///
/// Yields one borrowed `&str` per argument, quotes included, so a token can
/// never be half of a quoted path. Double quotes open and close a quoted run
/// and are kept in the emitted token, `""` inside a quoted run is one literal
/// quote, and a backslash inside a quoted run escapes the character after it.
///
/// Simplification: Windows itself only treats a backslash as an escape when it
/// is followed by one or more quotes, and counts a run of `2n` backslashes
/// before a quote as `n` literal backslashes. This iterator escapes a
/// backslash before *any* character and keeps the backslashes in the token,
/// which covers the two shapes an osu! command line actually contains (paths
/// with `\"` in them, and a quoted argument with an embedded quote).
pub struct CommandLineTokens<'a> {
    rest: &'a str,
}

impl<'a> CommandLineTokens<'a> {
    pub fn new(cmd: &'a str) -> Self {
        Self { rest: cmd }
    }
}

impl<'a> Iterator for CommandLineTokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let rest = self.rest;
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            self.rest = "";
            return None;
        }
        let start = i;
        let mut in_quotes = false;
        while i < bytes.len() {
            match bytes[i] {
                b'"' if in_quotes && bytes.get(i + 1) == Some(&b'"') => i += 2,
                b'"' => {
                    in_quotes = !in_quotes;
                    i += 1;
                }
                b'\\' if in_quotes && i + 1 < bytes.len() => i += 2,
                c if c.is_ascii_whitespace() && !in_quotes => break,
                _ => i += 1,
            }
        }
        self.rest = &rest[i..];
        Some(&rest[start..i])
    }
}

const TOURNAMENT_MANAGER_FLAGS: [&str; 4] = ["-tourney", "/tourney", "-tournament", "/tournament"];

/// Whether the command line marks osu! as a tournament manager.
///
/// The line is tokenized first and a flag is only recognised as a whole
/// argument, up to an optional `=value` suffix and with any enclosing quotes
/// stripped, so a song or replay path that merely contains the word —
/// `osu!.exe "D:\Songs\tournament_pack\map.osu"` — is not a manager, and
/// neither `-tournamentx` nor a bare `tournament` is.
///
/// `-go`/`/go` is deliberately absent: that is osu!'s **autoplay** flag, and
/// classifying a solo game as a tournament manager made the provider serve an
/// empty packet forever while reporting no error.
pub fn is_tournament_manager_cmd(cmd: &str) -> bool {
    CommandLineTokens::new(cmd).any(|token| {
        let token = token
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(token);
        let flag = token.split('=').next().unwrap_or(token);
        TOURNAMENT_MANAGER_FLAGS
            .iter()
            .any(|candidate| flag.eq_ignore_ascii_case(candidate))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModEntry {
    pub acronym: String,
}

/// Bit positions of osu!'s `Mods` bitfield, as the `u32` this crate reads out
/// of process memory (the `x ^ y` of osu!'s two mirrored mod fields).
///
/// Every constant is a single bit. A value that combines several mods is left as
/// a literal at the site that needs it, because naming only the individual bits
/// is what makes a combination readable. The bit order here is osu!'s, not a
/// sorted one: the key-count mods are scattered (`K4`..`K8` at 15..19, then
/// `FI`..`CN` at 20..22, then `K9`, `K10`, `K1`, `K3`, `K2` at 24..28), so the
/// grouping below follows `compute_format_mods`'s table rather than
/// alphabetising it.
pub mod mod_bits {
    pub const NF: u32 = 1 << 0;
    pub const EZ: u32 = 1 << 1;
    pub const TD: u32 = 1 << 2;
    pub const HD: u32 = 1 << 3;
    pub const HR: u32 = 1 << 4;
    pub const SD: u32 = 1 << 5;
    pub const DT: u32 = 1 << 6;
    pub const RX: u32 = 1 << 7;
    pub const HT: u32 = 1 << 8;
    pub const NC: u32 = 1 << 9;
    pub const FL: u32 = 1 << 10;
    pub const AT: u32 = 1 << 11;
    pub const SO: u32 = 1 << 12;
    pub const AP: u32 = 1 << 13;
    pub const PF: u32 = 1 << 14;
    /// osu!mania 4K.
    pub const K4: u32 = 1 << 15;
    pub const K5: u32 = 1 << 16;
    pub const K6: u32 = 1 << 17;
    pub const K7: u32 = 1 << 18;
    pub const K8: u32 = 1 << 19;
    pub const FI: u32 = 1 << 20;
    pub const RD: u32 = 1 << 21;
    pub const CN: u32 = 1 << 22;
    pub const TG: u32 = 1 << 23;
    pub const K9: u32 = 1 << 24;
    pub const K10: u32 = 1 << 25;
    pub const K1: u32 = 1 << 26;
    pub const K3: u32 = 1 << 27;
    pub const K2: u32 = 1 << 28;
    /// ScoreV2, set by osu! on a live score processor and not a gameplay mod.
    pub const SCORE_V2: u32 = 1 << 29;
    pub const MR: u32 = 1 << 30;
}

fn compute_format_mods(mods: u32) -> String {
    const VALUES: [(u32, &str, u8); 31] = [
        (mod_bits::NF, "NF", 99), // Replicate tosu quirk: ModsOrder['nf'] is 0 which is falsy in JS, defaulting order to 99
        (mod_bits::EZ, "EZ", 1),
        (mod_bits::TD, "TD", 7),
        (mod_bits::HD, "HD", 2),
        (mod_bits::HR, "HR", 4),
        (mod_bits::SD, "SD", 5),
        (mod_bits::DT, "DT", 3),
        (mod_bits::RX, "RX", 99),
        (mod_bits::HT, "HT", 3),
        (mod_bits::NC, "NC", 3),
        (mod_bits::FL, "FL", 6),
        (mod_bits::AT, "AT", 99),
        (mod_bits::SO, "SO", 5),
        (mod_bits::AP, "AP", 99),
        (mod_bits::PF, "PF", 5),
        (mod_bits::K4, "4K", 99),
        (mod_bits::K5, "5K", 99),
        (mod_bits::K6, "6K", 99),
        (mod_bits::K7, "7K", 99),
        (mod_bits::K8, "8K", 99),
        (mod_bits::FI, "FI", 99),
        (mod_bits::RD, "RD", 99),
        (mod_bits::CN, "CN", 99),
        (mod_bits::TG, "TG", 99),
        (mod_bits::K9, "9K", 99),
        (mod_bits::K10, "10K", 99),
        (mod_bits::K1, "1K", 99),
        (mod_bits::K3, "3K", 99),
        (mod_bits::K2, "2K", 99),
        (mod_bits::SCORE_V2, "v2", 99),
        (mod_bits::MR, "MR", 99),
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

pub fn format_mods(mods: u32) -> String {
    static CACHE: std::sync::LazyLock<std::sync::RwLock<std::collections::HashMap<u32, String>>> =
        std::sync::LazyLock::new(|| {
            std::sync::RwLock::new(std::collections::HashMap::with_capacity(32))
        });

    if let Ok(guard) = CACHE.read() {
        if let Some(s) = guard.get(&mods) {
            return s.clone();
        }
    }

    let result = compute_format_mods(mods);

    if let Ok(mut guard) = CACHE.write() {
        guard.insert(mods, result.clone());
    }
    result
}

pub fn mod_acronyms(mods: u32) -> Vec<ModEntry> {
    static CACHE: std::sync::LazyLock<
        std::sync::RwLock<std::collections::HashMap<u32, Vec<ModEntry>>>,
    > = std::sync::LazyLock::new(|| {
        std::sync::RwLock::new(std::collections::HashMap::with_capacity(32))
    });

    if let Ok(guard) = CACHE.read() {
        if let Some(entries) = guard.get(&mods) {
            return entries.clone();
        }
    }

    let s = format_mods(mods);
    let mut entries = Vec::new();
    let mut chars = s.chars().peekable();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        entries.push(ModEntry {
            acronym: format!("{a}{b}").to_uppercase(),
        });
    }

    if let Ok(mut guard) = CACHE.write() {
        guard.insert(mods, entries.clone());
    }
    entries
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
    // No beatmap context here, so `grade_max` falls back to the current grade;
    // this is a diagnostic collector, not a served payload.
    let gameplay = match read_gameplay_state(&memory, ruleset_address, 0) {
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
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(result);
                }
            });
        }
    });

    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

pub fn resolve_ruleset_container(
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
        let container_addr = match pattern_address.checked_sub(0xb) {
            Some(address) => address,
            None => continue,
        };
        let container = match memory.read_pointer(container_addr) {
            Ok(address) if address != 0 => address,
            _ => continue,
        };
        let ruleset_slot = match checked_add(container, 4) {
            Ok(address) => address,
            Err(_) => continue,
        };
        let candidate = match memory.read_pointer(ruleset_slot) {
            Ok(address) if address != 0 => address,
            _ => continue,
        };
        // Only the success of the read matters here, so the object count that
        // `grade_max` needs is irrelevant.
        if read_gameplay_state(memory, candidate, 0).is_ok()
            || read_tournament_state(memory, candidate).is_ok()
            || read_result_screen_state(memory, candidate).is_ok()
        {
            return Ok(container_addr);
        }
        fallback.get_or_insert(container_addr);
    }
    fallback.ok_or_else(|| anyhow::anyhow!("no valid ruleset container candidate was found"))
}

pub fn read_active_ruleset(memory: &ProcessMemory, container_addr: u64) -> Option<u64> {
    let container = memory.read_pointer(container_addr).ok()?;
    if container == 0 {
        return None;
    }
    let ruleset = memory.read_pointer(container.saturating_add(4)).ok()?;
    if ruleset == 0 {
        return None;
    }
    Some(ruleset)
}

pub fn resolve_ruleset(
    memory: &ProcessMemory,
    profile: &ClientProfile,
    scan_limit_bytes: usize,
) -> Result<u64> {
    let container_addr = resolve_ruleset_container(memory, profile, scan_limit_bytes)?;
    read_active_ruleset(memory, container_addr)
        .ok_or_else(|| anyhow::anyhow!("active ruleset is null"))
}

/// Score weight of an osu!mania MAX/rainbow 300 under the original ScoreV1 rules,
/// where a MAX is worth the same 300 points as a plain 300 (osu! wiki,
/// "Gameplay/Accuracy"; ppy/osu `ManiaScoreProcessor` before the ScoreV2 change).
const MANIA_MAX_SCORE_V1: f64 = 300.0;

/// Score weight of an osu!mania MAX/rainbow 300 under ScoreV2. In ppy/osu,
/// `ManiaScoreProcessor.GetBaseScoreForResult` returns 305 for
/// `HitResult.Perfect`, and `ScoreProcessor` gates accuracy on
/// `Judgement.MaxResult`, so a miss is also worth 305 in the denominator -- which
/// is why the weight multiplies `total` and not just the MAX count.
const MANIA_MAX_SCORE_V2: f64 = 305.0;

/// Per-ruleset accuracy as a percentage, from the raw result-screen judgement
/// counters (osu! wiki, "Gameplay/Accuracy").
///
/// osu! and taiko weight the judgements the way the score does. osu!catch does
/// not weight them at all: its counters are object counts, so accuracy is caught
/// objects over all objects, with `hit_miss` covering dropped fruit and drops and
/// `hit_katu` covering missed droplets. `hit_geki` is excluded from catch on
/// purpose -- the wiki states "countGeki should not be used to calculate the
/// accuracy at all", because it only counts caught combo-ending fruit, which is
/// already counted in `hit_300`.
///
/// osu!mania weights MAX/rainbow 300s (`hit_geki`) as the primary judgement,
/// which is why this takes the whole mod mask: the MAX weight is
/// `MANIA_MAX_SCORE_V1` unless the ScoreV2 bit (`mod_bits::SCORE_V2`) is set.
#[allow(clippy::too_many_arguments)]
pub fn calculate_accuracy(
    mode: i32,
    hit_300: i16,
    hit_100: i16,
    hit_50: i16,
    hit_miss: i16,
    hit_geki: i16,
    hit_katu: i16,
    mods: u32,
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
            let total = (hit_300 + hit_100 + hit_50 + hit_miss + hit_katu) as f64;
            if total == 0.0 {
                100.0
            } else {
                (hit_300 + hit_100 + hit_50) as f64 / total * 100.0
            }
        }
        3 => {
            let max_score = if (mods & mod_bits::SCORE_V2) != 0 {
                MANIA_MAX_SCORE_V2
            } else {
                MANIA_MAX_SCORE_V1
            };
            let total = (hit_geki + hit_300 + hit_katu + hit_100 + hit_50 + hit_miss) as f64;
            if total == 0.0 {
                100.0
            } else {
                (max_score * hit_geki as f64
                    + 300.0 * hit_300 as f64
                    + 200.0 * hit_katu as f64
                    + 100.0 * hit_100 as f64
                    + 50.0 * hit_50 as f64)
                    / (total * max_score)
                    * 100.0
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

    let accuracy = calculate_accuracy(
        mode, hit_300, hit_100, hit_50, hit_miss, hit_geki, hit_katu, mods,
    );
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

thread_local! {
    static HIT_ERROR_BYTE_BUFFER: std::cell::RefCell<Vec<u8>> =
        std::cell::RefCell::new(Vec::with_capacity(80_000));
    static HIT_ERROR_I16_BUFFER: std::cell::RefCell<Vec<i16>> =
        std::cell::RefCell::new(Vec::with_capacity(20_000));
}

pub fn read_hit_errors_arc(memory: &ProcessMemory, score_base: u64) -> Result<Arc<[i16]>> {
    crate::instr_scope!(HitErrors);
    let list = memory
        .read_pointer(checked_add(score_base, 0x38)?)
        .context("reading hit error list")?;
    if list == 0 {
        return Ok(Arc::default());
    }
    let items = memory
        .read_pointer(checked_add(list, 0x4)?)
        .context("reading hit error items")?;
    if items == 0 {
        return Ok(Arc::default());
    }
    let size = memory
        .read_i32(checked_add(list, 0xc)?)
        .context("reading hit error count")?;
    if size <= 0 {
        return Ok(Arc::default());
    }
    let (start, count) = hit_error_window(size as usize);
    let address = hit_error_items_address(items, start)?;

    HIT_ERROR_BYTE_BUFFER.with(|byte_cell| {
        HIT_ERROR_I16_BUFFER.with(|i16_cell| {
            let mut byte_buf = byte_cell.borrow_mut();
            byte_buf.resize(count * 4, 0);
            memory
                .read_into(address, &mut byte_buf)
                .with_context(|| format!("reading {count} hit errors at 0x{address:X}"))?;

            let mut i16_buf = i16_cell.borrow_mut();
            i16_buf.clear();
            for chunk in byte_buf.chunks_exact(4) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if !(-HIT_ERROR_BOUND..=HIT_ERROR_BOUND).contains(&value) {
                    break;
                }
                i16_buf.push(value.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
            }
            Ok(Arc::from(i16_buf.as_slice()))
        })
    })
}

pub fn read_hit_errors(memory: &ProcessMemory, score_base: u64) -> Result<Vec<i16>> {
    read_hit_errors_arc(memory, score_base).map(|arc| arc.to_vec())
}

/// Unstable rate from the live hit-error array, in osu!'s own units (the
/// hit-error standard deviation x 10, as a percentage).
///
/// A rate mod shortens the same hit errors in real time, so the raw spread has
/// to be divided by the clock rate to describe the play, not the wall clock.
/// Nightcore shares Double Time's branch: it is the same 1.5x clock, and osu!
/// stable normally sets the DT bit *alongside* NC — the `DTNC` -> `NC` collapse
/// in `compute_format_mods` only makes sense if both arrive — so testing NC on
/// its own is hardening rather than a user-visible fix.
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
    if (mods & mod_bits::DT) != 0 || (mods & mod_bits::NC) != 0 {
        rate / 1.5
    } else if (mods & mod_bits::HT) != 0 {
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
    let silver = (mods & mod_bits::HD) != 0 || (mods & mod_bits::FL) != 0;
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

/// tosu's `gameplay.gradeExpected` (`states/gameplay.ts:384-407`), which every
/// payload exposes as `maxThisPlay` / `max_this_play`.
///
/// **The name is a lie: this is not a maximum, it is a projection.** tosu
/// computes it as `calculateGrade` over a *hypothetical* statistics block in
/// which every object the player has not yet judged is counted as a 300:
///
/// ```text
/// great: statistics.great + objectCount
///              - statistics.great - statistics.ok - statistics.meh - statistics.miss
/// ```
///
/// i.e. `great + (objectCount - judged)`. Every other count and the accuracy are
/// left alone, so the result is "the grade this play would end on if nothing
/// else is missed".
///
/// Verified live on map 2964306 with 9 great / 6 ok / 0 meh / 1 miss of 604
/// objects: the current grade is `D` (`r300 = 9/16 = 0.5625`) and the projection
/// is `A` (`great` becomes 597, `r300 = 597/604 = 0.9884`, and the `r300 > 0.9`
/// arm fires despite the one miss). tosu served `maxThisPlay: 'A'`; rtosu served
/// `D`, because `grade_max` was a clone of the current grade.
///
/// `object_count` is `menu.objectCount`, read at `beatmap_addr + 0xF8`
/// (`memory/stable.ts:959`) -- the same field rtosu already reads at
/// `beatmap.rs:310` and stores as `beatmap.stats.objects.total`. An
/// `object_count` at or below the number already judged makes the projection
/// collapse to the current grade, which is what a caller with no beatmap
/// context should report.
pub fn calculate_tosu_grade_projected(
    mode: i32,
    hit_300: i16,
    hit_100: i16,
    hit_50: i16,
    hit_miss: i16,
    mods: u32,
    object_count: i32,
) -> String {
    let judged = hit_300 as i64 + hit_100 as i64 + hit_50 as i64 + hit_miss as i64;
    let remaining = object_count as i64 - judged;
    if remaining <= 0 {
        // Nothing left to judge: tosu's own expression collapses to
        // `great - ok - meh - miss`, which is nonsense, so the current grade is
        // the only defensible value.
        return calculate_tosu_grade(mode, 0.0, hit_300, hit_100, hit_50, hit_miss, mods);
    }
    let projected_300 = (hit_300 as i64 + remaining).clamp(0, i16::MAX as i64) as i16;
    // On stable, `calculateGrade` ignores `accuracy` entirely and works from the
    // statistics alone (`utils/calculators.ts:400-407` vs the lazer branch at
    // `:31-99`), so the accuracy argument here is inert. It is passed as `0.0`
    // rather than a real value so that nobody later reads it as meaningful.
    calculate_tosu_grade(mode, 0.0, projected_300, hit_100, hit_50, hit_miss, mods)
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

/// `object_count` is `menu.objectCount` (`beatmap_addr + 0xF8`), needed for
/// `grade_max`. Pass `0` when the caller has no beatmap context: the projection
/// then collapses to the current grade, which is the old behaviour rather than a
/// wrong number.
pub fn read_gameplay_state(
    memory: &ProcessMemory,
    ruleset_address: u64,
    object_count: i32,
) -> Result<GameplayState> {
    read_gameplay_state_cached(memory, ruleset_address, None, object_count)
}

pub fn read_gameplay_state_cached(
    memory: &ProcessMemory,
    ruleset_address: u64,
    cached_hits: Option<(u32, &Arc<[i16]>, f64)>,
    object_count: i32,
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
        mods |= mod_bits::SCORE_V2;
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
            let arr = read_hit_errors_arc(memory, score_base).unwrap_or_default();
            let ur = calculate_unstable_rate(&arr, mods);
            (arr, ur)
        }
    } else {
        let arr = read_hit_errors_arc(memory, score_base).unwrap_or_default();
        let ur = calculate_unstable_rate(&arr, mods);
        (arr, ur)
    };
    let grade = calculate_tosu_grade(mode, accuracy, hit_300, hit_100, hit_50, hit_miss, mods);
    // The projection, not a running maximum -- see
    // `calculate_tosu_grade_projected`. This used to be `grade.clone()`, which
    // reported the current grade and diverged from tosu on every play where the
    // grade had not yet fallen.
    let grade_max = calculate_tosu_grade_projected(
        mode,
        hit_300,
        hit_100,
        hit_50,
        hit_miss,
        mods,
        object_count,
    );

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
        key_overlay: read_key_overlay(memory, ruleset_address, mode),
    })
}

/// The osu! stable key overlay: which of the four key bindings are down, and how
/// many keys each has registered this play.
///
/// A port of `keyOverlay(mode)` in
/// `tosu-sourcecode/packages/tosu/src/memory/stable.ts:644-728`, against the
/// already-resolved `ruleset_address` -- the same base tosu reaches by its own
/// `[[patternAddr - 0xB] + 0x4]` walk, which is [`read_active_ruleset`]. No new
/// pattern scan is involved.
///
/// The walk, and every early exit, is tosu's:
///
/// ```text
/// keyOverlayPtr  = u32  (rulesetAddress + 0xAC)
/// arrayAddress   = i32 (i32 (keyOverlayPtr + 0x10) + 0x4)
/// itemsSize      = i32  (arrayAddress + 0x4)
/// element[i]     = i32  (arrayAddress + 0x8 + 4 * i)
/// isPressed      = u8   (element[i] + 0x1C)
/// count          = i32  (element[i] + 0x14)
/// ```
///
/// Three gates matter and all of them are reproduced rather than smoothed over:
///
/// * **`itemsSize < 4` returns an empty array.** Then every button falls back to
///   the neutral value in the payload builders, because they index `.at(0..3)`
///   with `?? false` / `?? 0`. That is why this returns a full four-button
///   struct rather than a `Vec`: the builders never see a short list.
/// * **A null `keyOverlayPtr` is mode-dependent.** tosu returns an empty string
///   (no key state) for mania and taiko, and an *error* for the others. Both
///   paths end up as "no key data" on the wire, so one neutral result covers
///   them, and the mode check is kept in a comment rather than in a branch that
///   cannot change the answer.
/// * **Only osu!std reads a fourth element.** Catch's three bindings are read as
///   `L`, `R`, `D` and taiko's as `K1`, `K2`, `M1`; `m2` exists only for
///   `mode == 0`. The *names* never reach the payload -- the builders address the
///   array positionally as `k1`, `k2`, `m1`, `m2` -- so the name difference is a
///   comment and the positional difference is the `mode == 0` element count.
///
/// Every failure path here returns the neutral overlay rather than propagating.
/// This runs inside the per-tick gameplay read, and tosu's own version answers
/// with an `Error` object that its callers then treat as an empty overlay -- so
/// propagating would make rtosu stricter than the implementation it is matching,
/// at the cost of dropping the whole gameplay block over a key that is not a
/// gameplay value.
pub fn read_key_overlay(
    memory: &ProcessMemory,
    ruleset_address: u64,
    mode: i32,
) -> crate::v2::KeyOverlay {
    let neutral = crate::v2::KeyOverlay::default();
    if ruleset_address == 0 {
        return neutral;
    }

    // `saturating_add` rather than the `checked_add(...)?` the other readers use:
    // this function answers with a neutral overlay rather than an `Err`, because
    // it runs inside the per-tick gameplay read and a key that is not a gameplay
    // value must not be able to fail the whole block. Saturation can only produce
    // `u64::MAX`, which is not a readable address, so the read below fails and the
    // button stays neutral -- the same answer, by a longer route.
    let at = |base: u64, offset: u64| base.saturating_add(offset);
    // tosu's reads are all `readInt`/`readUInt`, so every address in this walk is
    // a 32-bit value widened to 64. A null or saturated pointer is how a .NET
    // field that was never assigned shows up, and each one is a distinct early
    // exit in tosu, so they are rejected here rather than read from.
    fn pointer(value: u32) -> Option<u64> {
        (value != 0 && value != u32::MAX).then_some(value as u64)
    }

    let Some(key_overlay_ptr) = memory
        .read_u32(at(ruleset_address, 0xAC))
        .ok()
        .and_then(pointer)
    else {
        // tosu: a null pointer is "no key state" for taiko and mania, and an
        // error for everything else. Both reach the payload as the neutral
        // overlay.
        return neutral;
    };

    let Some(list) = memory
        .read_u32(at(key_overlay_ptr, 0x10))
        .ok()
        .and_then(pointer)
    else {
        return neutral;
    };
    let Some(array) = memory.read_u32(at(list, 0x4)).ok().and_then(pointer) else {
        return neutral;
    };

    // `itemsSize` is the array's length. tosu gates on `< 4` and returns an empty
    // list, which the builders turn into four neutral buttons.
    let Ok(items_size) = memory.read_i32(at(array, 0x4)) else {
        return neutral;
    };
    if items_size < 4 {
        return neutral;
    }

    // osu!std has a fourth binding; catch and taiko have three. tosu only pushes
    // the fourth element for `mode === 0`, and the builders still emit `m2`
    // (defaulted) for the three-element modes.
    let elements = if mode == 0 { 4 } else { 3 };

    let mut buttons = [crate::v2::KeyOverlayButton::default(); 4];
    for (index, button) in buttons.iter_mut().enumerate().take(elements) {
        let Some(element) = memory
            .read_u32(at(array, 0x8 + 4 * index as u64))
            .ok()
            .and_then(pointer)
        else {
            continue;
        };
        // `Boolean(byte)`, so any non-zero byte is pressed.
        if let Ok(pressed) = memory.read_u8(at(element, 0x1C)) {
            button.is_pressed = pressed != 0;
        }
        if let Ok(count) = memory.read_i32(at(element, 0x14)) {
            button.count = count;
        }
    }

    crate::v2::KeyOverlay {
        k1: buttons[0],
        k2: buttons[1],
        m1: buttons[2],
        m2: buttons[3],
    }
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
        CommandLineTokens, GameplayState, MAX_HIT_ERRORS, ProcessSnapshotResult,
        calculate_accuracy, calculate_tosu_grade, calculate_tosu_grade_projected,
        calculate_unstable_rate, compute_format_mods, format_mods, hit_error_items_address,
        hit_error_window, is_tournament_manager_cmd, mod_bits, parse_hit_errors,
        parse_spectate_client_arg,
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
        assert_eq!(format_mods(mod_bits::SCORE_V2), "v2");
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

    /// The FIX-003 regression: arguments are inspected whole, so a song or
    /// beatmap path that merely contains `tournament` or `-tourney` is not a
    /// manager, on its own or next to autoplay.
    #[test]
    fn tournament_inside_a_path_is_not_a_manager() {
        assert!(!is_tournament_manager_cmd(
            "osu!.exe \"D:\\Songs\\tournament\\map.osu\""
        ));
        assert!(!is_tournament_manager_cmd(
            "osu!.exe \"D:\\Songs\\tournament_pack\\map.osu\" -go"
        ));
        assert!(!is_tournament_manager_cmd(
            "osu!.exe \"C:\\Songs\\-tourney best\\map.osu\""
        ));
    }

    /// Only a complete flag argument counts. osu! takes the lobby address
    /// either as a separate argument or after `=`, while a longer word and a
    /// missing dash or slash are a different argument entirely.
    #[test]
    fn tournament_flag_must_be_a_whole_argument() {
        assert!(is_tournament_manager_cmd(
            "osu!.exe -tourney=127.0.0.1:24050"
        ));
        assert!(is_tournament_manager_cmd(
            "osu!.exe /tournament 127.0.0.1:24050"
        ));
        assert!(is_tournament_manager_cmd(
            "osu!.exe \"C:\\My Songs\\x.osu\" -tourney 127.0.0.1:24050"
        ));
        assert!(!is_tournament_manager_cmd("osu!.exe -tournamentx"));
        assert!(!is_tournament_manager_cmd("osu!.exe tournament"));
    }

    /// The quoting rules the matcher relies on: quotes group one argument,
    /// `""` inside a quoted run is one literal quote, and a backslash escapes
    /// the next character, so an escaped quote cannot close the run and
    /// release `-tourney` as an argument of its own.
    #[test]
    fn command_line_tokens_respect_quoting() {
        assert_eq!(
            CommandLineTokens::new("\"D:\\osu\\osu!.exe\" -tourney 127.0.0.1:24050")
                .collect::<Vec<_>>(),
            ["\"D:\\osu\\osu!.exe\"", "-tourney", "127.0.0.1:24050"]
        );
        assert_eq!(
            CommandLineTokens::new("  \"C:\\maps\\a b.osr\"  -replay ").collect::<Vec<_>>(),
            ["\"C:\\maps\\a b.osr\"", "-replay"]
        );
        assert_eq!(
            CommandLineTokens::new("osu!.exe \"C:\\Songs\\say \\\"hi\\\"\"").collect::<Vec<_>>(),
            ["osu!.exe", "\"C:\\Songs\\say \\\"hi\\\"\""]
        );
        assert_eq!(
            CommandLineTokens::new("osu!.exe \"C:\\Songs\\a\"\"b\\x.osu\"").collect::<Vec<_>>(),
            ["osu!.exe", "\"C:\\Songs\\a\"\"b\\x.osu\""]
        );
        assert_eq!(
            CommandLineTokens::new("").collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
        assert!(!is_tournament_manager_cmd(
            "osu!.exe \"C:\\Songs\\a\\\" -tourney\""
        ));
    }

    /// A flag still counts when it arrives wrapped in quotes, which is not how
    /// osu! launches it but costs nothing to accept. Only a token that is
    /// *exactly* the flag, once any enclosing quotes are stripped, matches --
    /// so a quoted path that merely contains one still does not.
    #[test]
    fn a_quoted_flag_still_counts_as_a_manager() {
        assert!(is_tournament_manager_cmd("osu!.exe \"-tourney\""));
        assert!(is_tournament_manager_cmd(
            "osu!.exe \"-tourney=127.0.0.1:24050\""
        ));
        assert!(!is_tournament_manager_cmd(
            "osu!.exe \"C:\\Songs\\-tourney\\map.osu\""
        ));
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

    /// FIX-027: Nightcore is the same 1.5x clock as Double Time, so `512` on
    /// its own has to divide the variance by 1.5 exactly as `64` does. The hits
    /// are picked so the arithmetic is checkable by hand instead of by
    /// comparing two calls to the same function -- mean 0, sum of squares 250
    /// over 5 hits, variance 50, `sqrt(50) * 10` = 70.71067811865476.
    #[test]
    fn nightcore_alone_scales_the_unstable_rate_like_double_time() {
        let hits = vec![-10i16, 10, 0, 5, -5];
        let raw = 50.0_f64.sqrt() * 10.0;
        assert!((raw - 70.71067811865476).abs() < 1e-9);

        assert!((calculate_unstable_rate(&hits, 0) - raw).abs() < 1e-9);
        assert!((calculate_unstable_rate(&hits, mod_bits::DT) - raw / 1.5).abs() < 1e-9);
        assert!((calculate_unstable_rate(&hits, mod_bits::NC) - raw / 1.5).abs() < 1e-9);
        assert!((calculate_unstable_rate(&hits, mod_bits::NC) - 47.14045207910317).abs() < 1e-9);
    }

    /// Adding Nightcore must not move anything else: nomod stays the raw rate,
    /// Half Time keeps its own 0.75x branch (so it *grows* the rate), and the
    /// `DTNC` pair osu! stable actually sets is still a single 1.5x -- not 2.25x
    /// from two independent halves.
    #[test]
    fn nightcore_leaves_nomod_half_time_and_the_dtnc_pair_alone() {
        let hits = vec![-10i16, 10, 0, 5, -5];
        let raw = 50.0_f64.sqrt() * 10.0;

        assert!((calculate_unstable_rate(&hits, mod_bits::HT) - raw / 0.75).abs() < 1e-9);
        assert!((calculate_unstable_rate(&hits, mod_bits::HT) - 94.28090415820635).abs() < 1e-9);
        assert!((calculate_unstable_rate(&hits, mod_bits::HT) - raw).abs() > 1e-9);

        let dtnc = mod_bits::DT | mod_bits::NC;
        assert!((calculate_unstable_rate(&hits, dtnc) - raw / 1.5).abs() < 1e-9);
        // Nightcore is checked first, so it wins over Half Time the same way
        // Double Time does.
        assert!(
            (calculate_unstable_rate(&hits, mod_bits::NC | mod_bits::HT) - raw / 1.5).abs() < 1e-9
        );
    }

    /// The value-preservation guard for the `mod_bits` swap: every bit osu!
    /// names must still format to exactly its own acronym, and the one bit it
    /// does not name (31) must still format to nothing. The masks here are
    /// written as raw integers on purpose -- reading them from the constants
    /// under test would make the assertion agree with any mis-numbering.
    #[test]
    fn every_named_mod_bit_still_formats_to_its_own_acronym() {
        let cases: &[(u32, &str)] = &[
            (0, ""),
            (1, "NF"),
            (2, "EZ"),
            (4, "TD"),
            (8, "HD"),
            (16, "HR"),
            (32, "SD"),
            (64, "DT"),
            (128, "RX"),
            (256, "HT"),
            (512, "NC"),
            (1024, "FL"),
            (2048, "AT"),
            (4096, "SO"),
            (8192, "AP"),
            (16384, "PF"),
            (1 << 15, "4K"),
            (1 << 16, "5K"),
            (1 << 17, "6K"),
            (1 << 18, "7K"),
            (1 << 19, "8K"),
            (1 << 20, "FI"),
            (1 << 21, "RD"),
            (1 << 22, "CN"),
            (1 << 23, "TG"),
            (1 << 24, "9K"),
            (1 << 25, "10K"),
            (1 << 26, "1K"),
            (1 << 27, "3K"),
            (1 << 28, "2K"),
            (1 << 29, "v2"),
            (1 << 30, "MR"),
            (1 << 31, ""),
        ];

        for &(mods, expected) in cases {
            assert_eq!(compute_format_mods(mods), expected, "mods {mods}");
            assert_eq!(format_mods(mods), expected, "mods {mods}");
        }
    }

    /// The `mod_bits` names have to point at osu!'s bit positions, not at a
    /// tidier re-numbering: the key-count mods are scattered around the mask
    /// (4K..8K at 15..19, then 9K/10K/1K/3K/2K at 24..28) and `1 << 20` is FI.
    #[test]
    fn mod_bits_hold_osus_bit_positions() {
        assert_eq!(mod_bits::NF, 1);
        assert_eq!(mod_bits::EZ, 2);
        assert_eq!(mod_bits::TD, 4);
        assert_eq!(mod_bits::HD, 8);
        assert_eq!(mod_bits::HR, 16);
        assert_eq!(mod_bits::SD, 32);
        assert_eq!(mod_bits::DT, 64);
        assert_eq!(mod_bits::RX, 128);
        assert_eq!(mod_bits::HT, 256);
        assert_eq!(mod_bits::NC, 512);
        assert_eq!(mod_bits::FL, 1024);
        assert_eq!(mod_bits::AT, 2048);
        assert_eq!(mod_bits::SO, 4096);
        assert_eq!(mod_bits::AP, 8192);
        assert_eq!(mod_bits::PF, 16384);
        assert_eq!(mod_bits::K4, 1 << 15);
        assert_eq!(mod_bits::K5, 1 << 16);
        assert_eq!(mod_bits::K6, 1 << 17);
        assert_eq!(mod_bits::K7, 1 << 18);
        assert_eq!(mod_bits::K8, 1 << 19);
        assert_eq!(mod_bits::FI, 1 << 20);
        assert_eq!(mod_bits::RD, 1 << 21);
        assert_eq!(mod_bits::CN, 1 << 22);
        assert_eq!(mod_bits::TG, 1 << 23);
        assert_eq!(mod_bits::K9, 1 << 24);
        assert_eq!(mod_bits::K10, 1 << 25);
        assert_eq!(mod_bits::K1, 1 << 26);
        assert_eq!(mod_bits::K3, 1 << 27);
        assert_eq!(mod_bits::K2, 1 << 28);
        assert_eq!(mod_bits::SCORE_V2, 1 << 29);
        assert_eq!(mod_bits::MR, 1 << 30);
    }

    /// The emitted string is a *concatenation* in a fixed order, so combinations
    /// are where a renamed bit or a re-sorted table would show up. Each case here
    /// is checked against the table by hand:
    ///
    /// - `DTNC` -> `NC` and `SDPF` -> `PF` are tosu's collapses of two mods that
    ///   osu! sets together; the tables give them the same sort order, so they
    ///   land adjacent.
    /// - `DTHTNC` is the *un*-collapsed form: HT sits between DT and NC, so the
    ///   `DTNC` substring is not there to replace. Pinned so a future
    ///   "simplification" of the collapse cannot change what is emitted.
    /// - The key-count groups follow the bit order, which is deliberately not
    ///   numerical: `1K3K2K` for bits 26/27/28 and `9K10K` for 24/25.
    /// - `PF4KFI` puts PF first because its sort order is 5 against the other
    ///   two mods' 99, and `4KMR` shows that a mask written high bit first still
    ///   emits in bit order.
    #[test]
    fn mod_combinations_still_collapse_and_order_the_same_way() {
        let cases: &[(u32, &str)] = &[
            (mod_bits::DT | mod_bits::NC, "NC"),
            (mod_bits::SD | mod_bits::PF, "PF"),
            (mod_bits::AT | mod_bits::CN, "CN"),
            (mod_bits::DT | mod_bits::HT | mod_bits::NC, "DTHTNC"),
            (mod_bits::NC | mod_bits::DT, "NC"),
            (
                mod_bits::K4 | mod_bits::K5 | mod_bits::K6 | mod_bits::K7 | mod_bits::K8,
                "4K5K6K7K8K",
            ),
            (mod_bits::K1 | mod_bits::K3 | mod_bits::K2, "1K3K2K"),
            (mod_bits::K9 | mod_bits::K10, "9K10K"),
            (
                mod_bits::FI | mod_bits::RD | mod_bits::CN | mod_bits::TG,
                "FIRDCNTG",
            ),
            (mod_bits::FI | mod_bits::PF | mod_bits::K4, "PF4KFI"),
            (mod_bits::MR | mod_bits::K4, "4KMR"),
            (
                mod_bits::HR | mod_bits::DT | mod_bits::FL | mod_bits::EZ,
                "EZDTHRFL",
            ),
            (mod_bits::SCORE_V2 | mod_bits::HD, "HDv2"),
            (mod_bits::MR | mod_bits::NF, "NFMR"),
        ];

        for &(mods, expected) in cases {
            assert_eq!(compute_format_mods(mods), expected, "mods {mods}");
            assert_eq!(format_mods(mods), expected, "mods {mods}");
        }
    }

    /// The one combined value with a live osu! reproduction behind it. It is a
    /// fixture, not a single mod bit, so it stays a literal -- but the name it
    /// emits must not move when its constituent bits gain names.
    #[test]
    fn the_live_validated_mod_mask_still_formats_identically() {
        assert_eq!(format_mods(536873225), "HDHTNFATv2");
    }

    #[test]
    fn test_i16_hit_error_json_serialization() {
        let hits: Vec<i16> = vec![-15, 0, 12, 35, -4];
        let json = serde_json::to_string(&hits).unwrap();
        // Serializes as standard JSON array of numbers, identical to tosu's Vec<i32> format
        assert_eq!(json, "[-15,0,12,35,-4]");
    }

    /// A perfect CtB run is 100%, not `300 * 100`. The old catch arm divided
    /// score weights by an unweighted total, so 300 caught fruits scored 30000%.
    #[test]
    fn catch_perfect_play_is_one_hundred_percent() {
        assert_eq!(calculate_accuracy(2, 300, 0, 0, 0, 0, 0, 0), 100.0);
    }

    /// 1000 caught fruits + 200 caught drops + 50 caught droplets = 1250 caught,
    /// over 1250 + 10 dropped + 5 missed droplets = 1265 objects.
    #[test]
    fn catch_accuracy_counts_objects_and_charges_missed_droplets_to_katu() {
        let accuracy = calculate_accuracy(2, 1000, 200, 50, 10, 0, 5, 0);
        assert_eq!(accuracy, 1250.0 / 1265.0 * 100.0);
        assert!((accuracy - 98.8142292490119).abs() < 1e-9);
    }

    /// `hit_geki` is caught combo-ending fruit, already counted in `hit_300`, so
    /// the wiki excludes it from catch accuracy entirely.
    #[test]
    fn catch_accuracy_ignores_geki() {
        let with_geki = calculate_accuracy(2, 1000, 200, 50, 10, 900, 5, 0);
        let without_geki = calculate_accuracy(2, 1000, 200, 50, 10, 0, 5, 0);
        assert_eq!(with_geki, without_geki);
        assert_eq!(with_geki, 1250.0 / 1265.0 * 100.0);
    }

    /// Under ScoreV1 a MAX is worth the same 300 as a plain 300, so an all-MAX
    /// play is 300 * n over 300 * n.
    #[test]
    fn mania_accuracy_of_an_all_max_play_is_one_hundred_percent_under_score_v1() {
        assert_eq!(calculate_accuracy(3, 0, 0, 0, 0, 1000, 0, 0), 100.0);
    }

    /// Under ScoreV2 the MAX weight is 305 in numerator *and* denominator, so an
    /// all-MAX play is 305 * n over 305 * n. Putting 305 only in the numerator
    /// would make this read 101.66666666666667%.
    #[test]
    fn mania_accuracy_of_an_all_max_play_is_one_hundred_percent_under_score_v2() {
        let accuracy = calculate_accuracy(3, 0, 0, 0, 0, 1000, 0, mod_bits::SCORE_V2);
        assert_eq!(accuracy, 100.0);
    }

    /// 1000 MAX + 500 300 + 200 200s + 100 100s + 50 50s + 20 misses = 1870
    /// objects. ScoreV1 numerator `300*1000 + 300*500 + 200*200 + 100*100 + 50*50`
    /// = 502500 over `300 * 1870` = 561000. ScoreV2 swaps the 300 MAX weight for
    /// 305: numerator 507500 over `305 * 1870` = 570350, which is a lower
    /// percentage because the miss is now also worth 305.
    #[test]
    fn mania_accuracy_matches_both_score_versions() {
        let v1 = calculate_accuracy(3, 500, 100, 50, 20, 1000, 200, 0);
        let v2 = calculate_accuracy(3, 500, 100, 50, 20, 1000, 200, mod_bits::SCORE_V2);

        assert_eq!(v1, 502_500.0 / 561_000.0 * 100.0);
        assert_eq!(v2, 507_500.0 / 570_350.0 * 100.0);
        assert!((v1 - 89.5721925133690).abs() < 1e-9);
        assert!((v2 - 88.9804506005085).abs() < 1e-9);
        assert_ne!(v1, v2);
        assert!(v1 > v2);
    }

    /// osu! and taiko already matched the wiki and must not drift: 800 300s + 150
    /// 100s + 50 50s + 20 misses is `300*800 + 100*150 + 50*50` = 257500 over
    /// `300 * 1020` = 306000, and 900 300s + 80 100s + 20 misses is
    /// `900 + 80*0.5` = 940 over 1000.
    #[test]
    fn osu_and_taiko_accuracy_are_unchanged() {
        let osu = calculate_accuracy(0, 800, 150, 50, 20, 0, 0, 0);
        let taiko = calculate_accuracy(1, 900, 80, 0, 20, 0, 0, 0);

        assert_eq!(osu, 257_500.0 / 306_000.0 * 100.0);
        assert!((osu - 84.1503267973856).abs() < 1e-9);
        assert_eq!(taiko, 94.0);
    }

    /// Every ruleset with no objects reads as a vacuous 100%; anything past the
    /// four rulesets is not an accuracy calculation at all.
    #[test]
    fn empty_hit_counts_read_as_one_hundred_percent_for_every_ruleset() {
        for mode in 0..4 {
            assert_eq!(calculate_accuracy(mode, 0, 0, 0, 0, 0, 0, 0), 100.0);
            assert_eq!(
                calculate_accuracy(mode, 0, 0, 0, 0, 0, 0, mod_bits::SCORE_V2),
                100.0
            );
        }
        assert_eq!(calculate_accuracy(4, 0, 0, 0, 0, 0, 0, 0), 0.0);
    }

    /// `grade_max` is a **projection**, not a running maximum, and getting that
    /// wrong is invisible until the grade has fallen below its starting value.
    ///
    /// Both numbers are transcribed from a live capture: map 2964306, osu!std,
    /// NF, 16 objects judged of 604. tosu served `rank.current: 'D'` and
    /// `rank.maxThisPlay: 'A'`; rtosu served `D` for both, because `grade_max`
    /// was a clone of the current grade.
    ///
    /// The arithmetic is tosu's own, from `states/gameplay.ts:393-407`:
    /// `great + objectCount - great - ok - meh - miss` = 9 + 588 = 597, so
    /// `r300` goes from 9/16 = 0.5625 to 597/604 = 0.98841 and the `r300 > 0.9`
    /// arm fires as `A` even though the play is not perfect and one object is a
    /// miss.
    #[test]
    fn the_projected_grade_is_not_the_current_grade() {
        assert_eq!(calculate_tosu_grade(0, 68.75, 9, 6, 0, 1, 0), "D");
        assert_eq!(
            calculate_tosu_grade_projected(0, 9, 6, 0, 1, 0, 604),
            "A",
            "the value tosu served for maxThisPlay"
        );

        // An untouched 604-300 play projects to itself.
        assert_eq!(calculate_tosu_grade(0, 100.0, 604, 0, 0, 0, 0), "X");
        assert_eq!(
            calculate_tosu_grade_projected(0, 604, 0, 0, 0, 0, 604),
            "X",
            "nothing left to judge: the projection is the current grade"
        );

        // Nothing judged at all: tosu's own expression subtracts more 300s than
        // exist, so the guard returns the current grade rather than a nonsense
        // ratio. This is also what a caller with no beatmap context gets.
        assert_eq!(calculate_tosu_grade_projected(0, 0, 0, 0, 0, 0, 604), "X");
        assert_eq!(calculate_tosu_grade_projected(0, 0, 0, 0, 0, 0, 0), "X");
    }

    /// A miss cannot be projected away, so the projection can never outrank what
    /// the misses allow. The `r50` and `miss == 0` gates mean a single miss
    /// blocks the S arm no matter how many 300s remain.
    #[test]
    fn the_projection_cannot_outrank_the_grade_a_miss_allows() {
        // 1 miss in 604: the S arm needs `miss == 0`, so the best it can reach
        // is A via the `r300 > 0.9` fallback. `great` projects to 603, so
        // `r300 = 603/604`.
        assert_eq!(calculate_tosu_grade_projected(0, 1, 0, 0, 1, 0, 604), "A");
        // The same play with no miss projects to 604/604, which is the `r300 == 1`
        // arm, so it reaches X rather than S.
        assert_eq!(calculate_tosu_grade_projected(0, 1, 0, 0, 0, 0, 604), "X");
        // Leaving one 100 unprojected keeps `r300` below 1, which is what makes
        // the S arm reachable: 599/604, no miss, no 50s.
        assert_eq!(calculate_tosu_grade_projected(0, 1, 5, 0, 0, 0, 604), "S");
        // 50s are not projected away either, so enough of them cap the grade at A
        // even with no miss at all: 584/604 with `r50 = 20/604`.
        assert_eq!(calculate_tosu_grade_projected(0, 0, 0, 20, 0, 0, 604), "A");
    }

    /// HD and FL turn X into XH and S into SH, on the projection as well as on
    /// the current grade -- `silver` is a property of the mods, not of the
    /// statistics.
    #[test]
    fn the_projection_honours_the_silver_mods() {
        let hd = mod_bits::HD;
        assert_eq!(calculate_tosu_grade_projected(0, 1, 5, 0, 0, 0, 604), "S");
        assert_eq!(calculate_tosu_grade_projected(0, 1, 5, 0, 0, hd, 604), "SH");
        assert_eq!(calculate_tosu_grade_projected(0, 1, 0, 0, 0, 0, 604), "X");
        assert_eq!(calculate_tosu_grade_projected(0, 1, 0, 0, 0, hd, 604), "XH");
    }

    /// osu!catch and osu!mania grade on **accuracy**, and tosu's projection never
    /// touches it -- it swaps only the statistics. On stable `calculateGrade`
    /// reads `params.accuracy` for those two modes
    /// (`utils/calculators.ts:51-99`), so the projection cannot be reproduced
    /// for them without also passing the accuracy through.
    ///
    /// `0.0` is passed deliberately: it makes the divergence visible as the
    /// accuracy floor rather than hiding it, and it is what osu!mania/catch
    /// report before any object is judged. Recorded rather than papered over.
    #[test]
    fn the_projection_is_inert_for_the_accuracy_driven_modes() {
        // Catch needs accuracy > 98 for S and > 94 for A.
        assert_eq!(calculate_tosu_grade(2, 99.0, 10, 5, 1, 0, 0), "S");
        assert_eq!(calculate_tosu_grade(2, 97.0, 10, 5, 1, 0, 0), "A");
        // Mania needs >= 95 for S.
        assert_eq!(calculate_tosu_grade(3, 97.0, 10, 5, 1, 0, 0), "S");
        // tosu returns the current grade here, because its accuracy is passed
        // through unchanged. rtosu returns the floor. Known divergence.
        assert_eq!(calculate_tosu_grade_projected(2, 10, 5, 1, 0, 0, 604), "D");
        assert_eq!(calculate_tosu_grade_projected(3, 10, 5, 1, 0, 0, 604), "D");
    }
}
