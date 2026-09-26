//! Live workload harness for `rtosu-dataprovider`.
//!
//! Drives the same `OsuReader` poll loop that `serve` uses, against a running
//! osu! process, and reports the numbers a flamegraph alone cannot give:
//! attach latency, per-poll service-time percentiles, achieved update rate and
//! scheduling drift. Optionally runs under the `dhat` allocator to attribute
//! allocations.
//!
//! Everything is configured through environment variables so the same binary
//! can be driven from PowerShell without a CLI dependency:
//!
//! | Variable | Default | Meaning |
//! | --- | --- | --- |
//! | `BENCH_LABEL` | `run` | name used in the output file |
//! | `BENCH_SECONDS` | `60` | measurement window (warmup excluded) |
//! | `BENCH_WARMUP_SECONDS` | `5` | warmup discarded before measuring |
//! | `BENCH_POLL_HZ` | `60` | configured poll rate, mirrors `serve` |
//! | `BENCH_MODE` | `auto` | `auto`, `solo` or `tournament` |
//! | `BENCH_PP` | `1` | `enable_pp` |
//! | `BENCH_CHAT` | `1` | `enable_chat` |
//! | `BENCH_SCAN_MB` | `128` | per-pattern scan budget in MiB |
//! | `BENCH_SERIALIZE` | `0` | also `serde_json::to_string` every packet |
//! | `BENCH_FIXED_RATE` | `0` | sleep only the remainder of the period |
//! | `BENCH_DHAT` | `0` | `1` runs the dhat allocation profiler |
//! | `BENCH_OUT_DIR` | `Benchmarks/results` | output directory |
//!
//! Usage:
//! ```powershell
//! cargo bench --bench live_harness
//! $env:BENCH_SECONDS = "180"; $env:BENCH_DHAT = "1"; cargo bench --bench live_harness
//! ```
//!
//! Requires a running osu! process; without one the reader reports
//! `notRunning` and attach latency is meaningless (it still reports `attached`).

use std::time::{Duration, Instant};

use rtosu_dataprovider::reader::{OsuReader, OsuReaderMode};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<T>().ok())
        .unwrap_or(default)
}

fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

struct Stats {
    samples_us: Vec<f64>,
    gaps_us: Vec<f64>,
    drift_us: Vec<f64>,
    errors: u64,
    serialized_bytes: u64,
    serialize_us: Vec<f64>,
}

impl Stats {
    fn new() -> Self {
        Self {
            samples_us: Vec::with_capacity(200_000),
            gaps_us: Vec::with_capacity(200_000),
            drift_us: Vec::with_capacity(200_000),
            errors: 0,
            serialized_bytes: 0,
            serialize_us: Vec::with_capacity(200_000),
        }
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

fn summarize(values: &mut [f64]) -> String {
    if values.is_empty() {
        return "{\"n\":0}".to_string();
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let sum: f64 = values.iter().sum();
    format!(
        "{{\"n\":{},\"mean\":{:.3},\"p50\":{:.3},\"p90\":{:.3},\"p99\":{:.3},\"p999\":{:.3},\"max\":{:.3},\"min\":{:.3}}}",
        values.len(),
        sum / values.len() as f64,
        percentile(values, 0.50),
        percentile(values, 0.90),
        percentile(values, 0.99),
        percentile(values, 0.999),
        values[values.len() - 1],
        values[0],
    )
}

fn is_live(packet: &rtosu_dataprovider::v2::TosuV2Packet) -> bool {
    !packet.state.name.is_empty() && (!packet.beatmap.title.is_empty() || packet.play.score > 0)
}

/// Slow the whole process down to model weaker hardware.
///
/// Two mechanisms, tried in order, because the obvious one does not work
/// everywhere:
///
/// 1. `SetProcessInformation(ProcessPowerThrottling)` with
///    `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`. This is the documented
///    per-process EcoQoS knob, but it is rejected with
///    `ERROR_INVALID_PARAMETER` (87) on Windows 11 Pro 26100.1, so
///    `BENCH_THROTTLE_PCT` silently did nothing on this machine until this
///    fallback was added.
/// 2. A job object with `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`, which makes the
///    scheduler hold the process to `percent` of one core. Same observable
///    effect for this workload (one CPU-bound thread doing a fixed number of
///    `ReadProcessMemory` calls per tick), and it is a hard guarantee rather
///    than a best-effort hint.
///
/// Returns `(applied, mechanism, last_os_error)`.
fn apply_cpu_throttle(percent: u8) -> (bool, &'static str, i32) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
            JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
            JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0, JobObjectCpuRateControlInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
        };

        let errno = |e: std::io::Error| e.raw_os_error().unwrap_or(-1);

        // 1. EcoQoS per-process execution-speed cap.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: 1,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: percent as u32,
        };
        let size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;
        let ok = unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                &state as *const _ as *const c_void,
                size,
            )
        };
        if ok != 0 {
            return (true, "process-power-throttling", 0);
        }
        let ecoqos_error = errno(std::io::Error::last_os_error());

        // 2. Job-object CPU hard cap. CpuRate is in 1/100 of a percent.
        let rate = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 {
                CpuRate: percent as u32 * 100,
            },
        };
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return (false, "none", ecoqos_error);
        }
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectCpuRateControlInformation,
                &rate as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            let err = errno(std::io::Error::last_os_error());
            unsafe { CloseHandle(job) };
            return (
                false,
                "none",
                if ecoqos_error != 0 { ecoqos_error } else { err },
            );
        }
        let assigned = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
        // The handle is intentionally leaked: the job must stay alive for the
        // lifetime of the process, and dropping it would end the throttling.
        let err = if assigned == 0 {
            errno(std::io::Error::last_os_error())
        } else {
            0
        };
        (assigned != 0, "job-object-cpu-hard-cap", err)
    }
    #[cfg(not(windows))]
    {
        let _ = percent;
        (false, "unsupported", 0)
    }
}

fn main() {
    let label = std::env::var("BENCH_LABEL").unwrap_or_else(|_| "run".to_string());
    let seconds = env_or("BENCH_SECONDS", 60.0f64);
    let warmup_seconds = env_or("BENCH_WARMUP_SECONDS", 5.0f64);
    let poll_hz = env_or("BENCH_POLL_HZ", 60.0f64).max(0.1);
    let period = Duration::from_secs_f64(1.0 / poll_hz);
    let scan_mb = env_or("BENCH_SCAN_MB", 128usize);
    let serialize = env_flag("BENCH_SERIALIZE", false);
    let fixed_rate = env_flag("BENCH_FIXED_RATE", false);
    let want_dhat = env_flag("BENCH_DHAT", false);
    let out_dir =
        std::env::var("BENCH_OUT_DIR").unwrap_or_else(|_| "Benchmarks/results".to_string());
    let mode = match std::env::var("BENCH_MODE").unwrap_or_default().as_str() {
        "solo" => OsuReaderMode::Solo,
        "tournament" => OsuReaderMode::Tournament,
        _ => OsuReaderMode::Auto,
    };
    let enable_pp = env_flag("BENCH_PP", true);
    let enable_chat = env_flag("BENCH_CHAT", true);

    std::fs::create_dir_all(&out_dir).ok();

    let throttle_pct = env_or("BENCH_THROTTLE_PCT", 100u8);
    let (throttled, throttle_mechanism, throttle_error) = if throttle_pct < 100 {
        let (ok, mech, err) = apply_cpu_throttle(throttle_pct);
        eprintln!(
            "cpu throttling {throttle_pct}% requested, applied={ok} via {mech} (errno {err})"
        );
        (ok, mech, err)
    } else {
        (false, "none", 0)
    };

    #[cfg(feature = "dhat-heap")]
    let dhat_profiler: Option<dhat::Profiler> = if want_dhat {
        let path = format!("{out_dir}/dhat-heap_{label}.json");
        // No `.testing()`: that mode keeps the data in memory and never writes
        // the file, so the profile would be lost when the profiler is dropped.
        Some(dhat::Profiler::builder().file_name(&path).build())
    } else {
        None
    };
    #[cfg(not(feature = "dhat-heap"))]
    let dhat_profiler: Option<()> = {
        if want_dhat {
            eprintln!("BENCH_DHAT=1 requires --features dhat-heap; continuing without dhat");
        }
        None
    };

    let built_at = Instant::now();
    let mut reader = match OsuReader::builder()
        .mode(mode)
        .poll_interval(period)
        .scan_limit_bytes(scan_mb * 1024 * 1024)
        .enable_pp(enable_pp)
        .enable_chat(enable_chat)
        .build()
    {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("failed to build reader: {err:#}");
            std::process::exit(1);
        }
    };
    let build_ms = built_at.elapsed().as_secs_f64() * 1000.0;

    // Phase 1: attach. The reader resolves patterns by scanning osu!'s address
    // space, so the first live packet can be far later than process start: the
    // attach budget must exceed the gradual PP chunk build (145 s measured on a
    // 47 000-object marathon map, and longer for bigger ones).
    let attach_budget = env_or("BENCH_ATTACH_BUDGET", 300.0f64);
    let attach_started = Instant::now();
    let mut attach_ms: Option<f64> = None;
    let mut first_poll_ms = Vec::new();
    let mut attached = false;
    while attach_started.elapsed().as_secs_f64() < attach_budget && attach_ms.is_none() {
        let t0 = Instant::now();
        if let Ok(packet) = reader.poll()
            && is_live(&packet)
        {
            attach_ms = Some(attach_started.elapsed().as_secs_f64() * 1000.0);
            attached = true;
        }
        first_poll_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(period);
    }
    let attach_total_ms = attach_started.elapsed().as_secs_f64() * 1000.0;

    // Phase 2: warmup (allocator growth, PP cache, JIT-less codegen paths).
    let warmup_deadline = Instant::now() + Duration::from_secs_f64(warmup_seconds);
    while Instant::now() < warmup_deadline {
        let _ = reader.poll();
        std::thread::sleep(period);
    }

    // Phase 3: measurement.
    let mut stats = Stats::new();
    let wall_started = Instant::now();
    let measure_deadline = wall_started + Duration::from_secs_f64(seconds);
    let mut last_start = Instant::now();
    let mut expected = wall_started;
    let mut packets = 0u64;
    let mut last_score = 0i32;
    let mut last_combo = 0i32;
    let mut last_hit_error_count = 0usize;
    let mut last_unstable_rate = 0.0f64;
    let mut hit_error_counts: Vec<f64> = Vec::with_capacity(200_000);
    let mut unstable_rates: Vec<f64> = Vec::with_capacity(200_000);
    let mut pp_samples = 0f64;
    let mut pp_count = 0u64;

    while Instant::now() < measure_deadline {
        let poll_start = Instant::now();
        stats
            .drift_us
            .push((poll_start - expected).as_secs_f64() * 1e6);
        let t0 = Instant::now();
        match reader.poll() {
            Ok(packet) => {
                let dt = t0.elapsed();
                stats.samples_us.push(dt.as_secs_f64() * 1e6);
                stats
                    .gaps_us
                    .push((poll_start - last_start).as_secs_f64() * 1e6);
                last_start = poll_start;
                packets += 1;
                last_score = packet.play.score;
                last_combo = packet.play.combo.current;
                last_hit_error_count = packet.play.hit_error_array.len();
                last_unstable_rate = packet.play.unstable_rate;
                hit_error_counts.push(last_hit_error_count as f64);
                if packet.play.unstable_rate > 0.0 {
                    unstable_rates.push(packet.play.unstable_rate);
                }
                if packet.play.pp.current > 0.0 {
                    pp_samples += packet.play.pp.current as f64;
                    pp_count += 1;
                }
                if serialize {
                    let s0 = Instant::now();
                    if let Ok(json) = serde_json::to_string(&packet) {
                        stats.serialized_bytes += json.len() as u64;
                        stats.serialize_us.push(s0.elapsed().as_secs_f64() * 1e6);
                    }
                }
            }
            Err(_) => {
                stats.errors += 1;
            }
        }

        if fixed_rate {
            let elapsed_in_period = poll_start.elapsed();
            if elapsed_in_period < period {
                std::thread::sleep(period - elapsed_in_period);
            }
        } else {
            // Mirrors `serve`: sleep after the poll, so the effective period is
            // poll_time + interval and the configured rate is not honoured.
            std::thread::sleep(period);
        }
        expected += period;
    }

    let wall = wall_started.elapsed().as_secs_f64();
    #[cfg(feature = "instr")]
    let instr_json: serde_json::Value =
        serde_json::from_str(&rtosu_dataprovider::instr::snapshot_json()).unwrap_or_default();
    #[cfg(not(feature = "instr"))]
    let instr_json: serde_json::Value = serde_json::Value::Null;

    let unstable_n = unstable_rates.len();
    let unstable_mean = if unstable_n == 0 {
        0.0
    } else {
        unstable_rates.iter().sum::<f64>() / unstable_n as f64
    };

    let packet_json = serde_json::json!({
        "label": label,
        "attached": attached,
        "attach_ms": attach_ms,
        "attach_budget_exhausted_ms": attach_total_ms,
        "reader_build_ms": build_ms,
        "first_polls_ms": first_poll_ms,
        "config": {
            "poll_hz": poll_hz,
            "scan_mb": scan_mb,
            "mode": format!("{mode:?}"),
            "enable_pp": enable_pp,
            "enable_chat": enable_chat,
            "serialize": serialize,
            "fixed_rate": fixed_rate,
            "dhat": want_dhat,
            "throttle_pct": throttle_pct,
            "throttle_applied": throttled,
            "throttle_mechanism": throttle_mechanism,
            "throttle_errno": throttle_error,
        },
        "wall_seconds": wall,
        "packets": packets,
        "poll_errors": stats.errors,
        "achieved_hz": if wall > 0.0 { packets as f64 / wall } else { 0.0 },
        "cpu_budget_pct_of_one_core": if wall > 0.0 {
            stats.samples_us.iter().sum::<f64>() / 1e6 / wall * 100.0
        } else { 0.0 },
        "poll_service_time_us": summarize(&mut stats.samples_us),
        "poll_period_us": summarize(&mut stats.gaps_us),
        "schedule_drift_us": summarize(&mut stats.drift_us),
        "serialize_time_us": summarize(&mut stats.serialize_us),
        "serialized_bytes_total": stats.serialized_bytes,
        "serialized_bytes_per_packet": stats.serialized_bytes.checked_div(packets).unwrap_or(0),
        "last_score": last_score,
        "last_combo": last_combo,
        "last_hit_error_count": last_hit_error_count,
        "last_unstable_rate": last_unstable_rate,
        "hit_error_count": summarize(&mut hit_error_counts),
        "unstable_rate_when_nonzero": {
            "n": unstable_n,
            "mean": unstable_mean,
        },
        "pp_mean": if pp_count > 0 { pp_samples / pp_count as f64 } else { 0.0 },
        "instr": instr_json,
    });

    let out_path = format!("{out_dir}/live_harness_{label}.json");
    std::fs::write(
        &out_path,
        format!("{}\n", serde_json::to_string_pretty(&packet_json).unwrap()),
    )
    .expect("write result json");
    println!("{}", serde_json::to_string_pretty(&packet_json).unwrap());
    eprintln!("wrote {out_path}");

    #[cfg(feature = "dhat-heap")]
    if let Some(profiler) = dhat_profiler {
        let stats = dhat::HeapStats::get();
        eprintln!(
            "dhat: total_blocks={} total_bytes={} curr_blocks={} curr_bytes={} max_blocks={} max_bytes={}",
            stats.total_blocks,
            stats.total_bytes,
            stats.curr_blocks,
            stats.curr_bytes,
            stats.max_blocks,
            stats.max_bytes,
        );
        drop(profiler);
    }
    #[cfg(not(feature = "dhat-heap"))]
    let _ = dhat_profiler;
}
