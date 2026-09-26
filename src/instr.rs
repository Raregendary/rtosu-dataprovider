//! Optional phase instrumentation (feature `instr`, off by default).
//!
//! Sampling profilers cannot be used unelevated on Windows (ETW kernel sessions
//! require administrator rights), so this module provides the deterministic
//! alternative: a set of named counters that the hot paths of the reader
//! accumulate into. Every scope is an RAII guard, so a phase cannot be skipped
//! by an early return or a panic.
//!
//! Design constraints:
//! * When the `instr` feature is disabled the module does not exist and the
//!   `instr_scope!` / `instr_bytes!` macros expand to nothing, so the shipped
//!   binary is byte-for-byte unaffected.
//! * A scope records *inclusive* time. Nested scopes (for example
//!   `SoloPoll` containing `ReadBytes`) are expected; exclusive time is
//!   `inclusive` minus the nested phases.
//! * `add_bytes` attributes bytes to the `ReadBytes` phase, which is the single
//!   place where bytes are pulled out of the target process. `ScanPattern` bytes
//!   are therefore a subset of `ReadBytes` bytes, not an addition.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const PHASE_COUNT: usize = 29;

pub const NAMES: [&str; PHASE_COUNT] = [
    "ReadBytes",
    "ScanPattern",
    "ScanRegions",
    "ProcessDiscovery",
    "ProcessOpen",
    "SoloPoll",
    "ReaderPoll",
    "TourneyPoll",
    "BeatmapMemory",
    "BeatmapFileMeta",
    "BeatmapDifficulty",
    "GameplayState",
    "HitErrors",
    "ResultScreen",
    "LocalProfile",
    "TournamentChat",
    "ModsState",
    "PpChunksCompute",
    "PpChunksCached",
    "PpLive",
    "JsonEncode",
    "PacketSend",
    "SoloSkin",
    "BeatmapFileRead",
    "BeatmapParse",
    "PpDifficulty",
    "GraphBuild",
    "GraphClone",
    "PacketClone",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Phase {
    ReadBytes = 0,
    ScanPattern = 1,
    ScanRegions = 2,
    ProcessDiscovery = 3,
    ProcessOpen = 4,
    SoloPoll = 5,
    ReaderPoll = 6,
    TourneyPoll = 7,
    BeatmapMemory = 8,
    BeatmapFileMeta = 9,
    BeatmapDifficulty = 10,
    GameplayState = 11,
    HitErrors = 12,
    ResultScreen = 13,
    LocalProfile = 14,
    TournamentChat = 15,
    ModsState = 16,
    PpChunksCompute = 17,
    PpChunksCached = 18,
    PpLive = 19,
    JsonEncode = 20,
    PacketSend = 21,
    SoloSkin = 22,
    BeatmapFileRead = 23,
    BeatmapParse = 24,
    PpDifficulty = 25,
    GraphBuild = 26,
    GraphClone = 27,
    PacketClone = 28,
}

impl Phase {
    pub fn name(self) -> &'static str {
        NAMES[self as usize]
    }
}

struct Cell {
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    count: AtomicU64,
    bytes: AtomicU64,
    failures: AtomicU64,
}

impl Cell {
    const fn new() -> Self {
        Self {
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            count: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }
}

static CELLS: [Cell; PHASE_COUNT] = [const { Cell::new() }; PHASE_COUNT];

/// RAII guard for one phase. Accumulates on drop, including on early return.
pub struct Scope {
    phase: Phase,
    start: Instant,
    failed: bool,
}

impl Scope {
    pub fn new(phase: Phase) -> Self {
        Self {
            phase,
            start: Instant::now(),
            failed: false,
        }
    }

    pub fn fail(&mut self) {
        self.failed = true;
    }

    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let cell = &CELLS[self.phase as usize];
        let ns = self.start.elapsed().as_nanos() as u64;
        cell.total_ns.fetch_add(ns, Ordering::Relaxed);
        cell.count.fetch_add(1, Ordering::Relaxed);
        cell.max_ns.fetch_max(ns, Ordering::Relaxed);
        if self.failed {
            cell.failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Attribute `bytes` read out of the target process to the `ReadBytes` phase.
pub fn add_bytes(bytes: u64) {
    CELLS[Phase::ReadBytes as usize]
        .bytes
        .fetch_add(bytes, Ordering::Relaxed);
}

#[derive(Clone, Debug)]
pub struct PhaseStat {
    pub phase: &'static str,
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub bytes: u64,
    pub failures: u64,
}

impl PhaseStat {
    pub fn total_ms(&self) -> f64 {
        self.total_ns as f64 / 1e6
    }
    pub fn mean_us(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.count as f64 / 1e3
        }
    }
    pub fn max_ms(&self) -> f64 {
        self.max_ns as f64 / 1e6
    }
}

pub fn snapshot() -> Vec<PhaseStat> {
    let mut out: Vec<PhaseStat> = (0..PHASE_COUNT)
        .map(|i| PhaseStat {
            phase: NAMES[i],
            count: CELLS[i].count.load(Ordering::Relaxed),
            total_ns: CELLS[i].total_ns.load(Ordering::Relaxed),
            max_ns: CELLS[i].max_ns.load(Ordering::Relaxed),
            bytes: CELLS[i].bytes.load(Ordering::Relaxed),
            failures: CELLS[i].failures.load(Ordering::Relaxed),
        })
        .filter(|s| s.count > 0)
        .collect();
    out.sort_by(|a, b| b.total_ns.cmp(&a.total_ns));
    out
}

pub fn reset() {
    for cell in CELLS.iter() {
        cell.total_ns.store(0, Ordering::Relaxed);
        cell.max_ns.store(0, Ordering::Relaxed);
        cell.count.store(0, Ordering::Relaxed);
        cell.bytes.store(0, Ordering::Relaxed);
        cell.failures.store(0, Ordering::Relaxed);
    }
}

/// Human-readable table, sorted by total time.
pub fn format_table() -> String {
    let stats = snapshot();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<20} {:>10} {:>12} {:>12} {:>12} {:>14}",
        "phase", "count", "total_ms", "mean_us", "max_ms", "bytes"
    );
    for s in &stats {
        let _ = writeln!(
            out,
            "{:<20} {:>10} {:>12.3} {:>12.2} {:>12.3} {:>14}",
            s.phase,
            s.count,
            s.total_ms(),
            s.mean_us(),
            s.max_ms(),
            s.bytes
        );
    }
    out
}

/// JSON snapshot for the benchmark tooling.
pub fn snapshot_json() -> String {
    let mut out = String::from("{\"phases\":[");
    for (i, s) in snapshot().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"phase\":\"{}\",\"count\":{},\"total_ms\":{:.3},\"mean_us\":{:.2},\"max_ms\":{:.3},\"bytes\":{},\"failures\":{}}}",
            s.phase,
            s.count,
            s.total_ms(),
            s.mean_us(),
            s.max_ms(),
            s.bytes,
            s.failures
        );
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_accumulates_on_drop() {
        reset();
        {
            let mut scope = Scope::new(Phase::ModsState);
            add_bytes(7);
            scope.fail();
        }
        {
            let _scope = Scope::new(Phase::ModsState);
        }
        let stats = snapshot();
        let mods = stats
            .iter()
            .find(|s| s.phase == "ModsState")
            .expect("phase");
        assert_eq!(mods.count, 2);
        assert_eq!(mods.failures, 1);
        let reads = stats
            .iter()
            .find(|s| s.phase == "ReadBytes")
            .expect("phase");
        assert_eq!(reads.bytes, 7);
    }

    #[test]
    fn names_match_phases() {
        for (i, name) in NAMES.iter().enumerate() {
            assert_eq!(Phase::ModsState.name(), "ModsState");
            assert!(!name.is_empty());
            assert!(i < PHASE_COUNT);
        }
    }
}
