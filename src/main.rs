use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rtosu_dataprovider::address::{
    checked_add, checked_add_signed, format_address, parse_i64, parse_u64, parse_u128_as_u64,
};
use rtosu_dataprovider::client::{snapshot_process, snapshot_processes};
use rtosu_dataprovider::config::{AppConfig, DEFAULT_CONFIG_FILE};
use rtosu_dataprovider::pattern::BytePattern;
use rtosu_dataprovider::process::{ProcessMemory, list_modules, list_processes, module_or_main};
use rtosu_dataprovider::profile::{available_profiles, load_profile};
use rtosu_dataprovider::session::TournamentSession;
use rtosu_dataprovider::tournament::read_tournament_state;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "rtosu_dataprovider",
    version,
    about = "High-performance osu! process memory reader and tosu replacement"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(short, long, global = true, help = "Path to custom configuration file")]
    config: Option<String>,
    #[arg(long, global = true, value_parser = parse_pointer_width)]
    pointer_width: Option<usize>,
}

#[derive(Subcommand)]
enum Command {
    Processes {
        #[arg(long)]
        name: Option<String>,
    },
    Modules {
        pid: u32,
    },
    Regions {
        pid: u32,
        #[arg(long)]
        all: bool,
    },
    Read {
        pid: u32,
        address: String,
        #[arg(long)]
        module: Option<String>,
        #[arg(long, default_value_t = 8)]
        length: usize,
    },
    Chain {
        pid: u32,
        module: String,
        #[arg(allow_hyphen_values = true)]
        base_offset: String,
        #[arg(required = true, value_delimiter = ',', allow_hyphen_values = true)]
        offsets: Vec<String>,
        #[arg(long)]
        read: bool,
    },
    Snapshot {
        pid: u32,
        #[arg(default_value = "tournament")]
        profile: String,
        #[arg(long, default_value_t = 128)]
        scan_megabytes: usize,
    },
    SnapshotAll {
        #[arg(default_value = "tournament")]
        profile: String,
        #[arg(long, default_value_t = 128)]
        scan_megabytes: usize,
    },
    TournamentPoll {
        #[arg(default_value = "tournament")]
        profile: String,
        #[arg(long, default_value_t = 128)]
        scan_megabytes: usize,
    },
    TournamentWatch {
        #[arg(default_value = "tournament")]
        profile: String,
        #[arg(long, default_value_t = 100)]
        interval_ms: u64,
        #[arg(long, default_value_t = 30)]
        count: usize,
    },
    CompareTosu {
        #[arg(long, default_value = "http://127.0.0.1:24050/json/v2")]
        url: String,
    },
    Scan {
        pid: u32,
        pattern: String,
        #[arg(long)]
        module: Option<String>,
        #[arg(long, default_value_t = 100)]
        max_matches: usize,
        #[arg(long, default_value_t = 128)]
        max_megabytes: usize,
    },
    ScanProfile {
        pid: u32,
        profile: String,
        key: String,
        #[arg(long)]
        module: Option<String>,
        #[arg(long, default_value_t = 100)]
        max_matches: usize,
        #[arg(long, default_value_t = 128)]
        max_megabytes: usize,
    },
    TournamentState {
        pid: u32,
        ruleset_address: String,
    },
    ResolveRuleset {
        pid: u32,
        profile: String,
    },
    Watch {
        pid: u32,
        address: String,
        #[arg(value_enum)]
        kind: ValueKind,
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
    },
    Profile {
        #[arg(default_value = "all")]
        name: String,
    },
    #[cfg(feature = "rosu-mem")]
    RosuMem {
        process: String,
        address: String,
        #[arg(long, default_value_t = 8)]
        length: usize,
    },
    #[cfg(feature = "rosu-mem")]
    RosuMemSignature {
        process: String,
        pattern: String,
    },
    /// View, validate, or generate configuration file
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Run the high-performance drop-in tosu HTTP & WebSocket streaming server
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        poll_rate: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Display the currently active configuration
    Show,
    /// Create a fresh documented config.toml file
    Init {
        #[arg(default_value = DEFAULT_CONFIG_FILE)]
        path: String,
    },
    /// Validate the active configuration file
    Validate,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ValueKind {
    U8,
    U32,
    I32,
    U64,
    F32,
    F64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.as_deref().unwrap_or(DEFAULT_CONFIG_FILE);
    let config = AppConfig::load_or_init(config_path)?;
    rtosu_dataprovider::logging::init_logging(&config.logging)?;
    let command = cli.command.unwrap_or(Command::Serve {
        host: None,
        port: None,
        poll_rate: None,
    });
    execute(command, cli.pointer_width, config, cli.config.as_deref())
}

fn execute(
    command: Command,
    pointer_width: Option<usize>,
    config: AppConfig,
    custom_config_path: Option<&str>,
) -> Result<()> {
    match command {
        Command::Processes { name } => {
            let processes = list_processes(name.as_deref())?;
            println!("PID\tPROCESS\tCOMMAND LINE");
            for process in processes {
                let cmd = ProcessMemory::open(process.pid)
                    .and_then(|m| m.command_line())
                    .unwrap_or_default();
                println!("{}\t{}\t{}", process.pid, process.name, cmd);
            }
        }
        Command::Modules { pid } => {
            for module in list_modules(pid)? {
                println!(
                    "{}\t0x{:X}\t0x{:X}\t{}",
                    module.name,
                    module.base,
                    module.size,
                    module.path.as_deref().unwrap_or("")
                );
            }
        }
        Command::Regions { pid, all } => {
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
            for region in memory.query_regions()? {
                if !all && !region.is_readable() {
                    continue;
                }
                println!(
                    "0x{:016X}\t0x{:X}\tstate=0x{:X}\tprotect=0x{:X}\ttype=0x{:X}",
                    region.base, region.size, region.state, region.protection, region.kind
                );
            }
        }
        Command::Read {
            pid,
            address,
            module,
            length,
        } => {
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
            let address = resolve_read_address(pid, &address, module.as_deref())?;
            let bytes = memory.read_bytes(address, length)?;
            print_bytes(&bytes);
        }
        Command::Chain {
            pid,
            module,
            base_offset,
            offsets,
            read,
        } => {
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
            let modules = list_modules(pid)?;
            let module = module_or_main(&modules, &module)?;
            let base = checked_add_signed(module.base, parse_i64(&base_offset)?)?;
            let offsets = offsets
                .iter()
                .map(|offset| parse_i64(offset))
                .collect::<Result<Vec<_>>>()?;
            let final_address = memory.resolve_pointer_chain_signed(base, &offsets)?;
            println!(
                "module={} base={} final={}",
                module.name,
                format_address(base),
                format_address(final_address)
            );
            if read {
                println!("value=0x{:X}", memory.read_pointer(final_address)?);
            }
        }
        Command::Snapshot {
            pid,
            profile,
            scan_megabytes,
        } => {
            let limit = scan_limit_bytes(scan_megabytes)?;
            let snapshot = snapshot_process(pid, &profile, pointer_width, limit)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Command::SnapshotAll {
            profile,
            scan_megabytes,
        } => {
            let limit = scan_limit_bytes(scan_megabytes)?;
            let snapshots = snapshot_processes(&profile, pointer_width, limit)?;
            println!("{}", serde_json::to_string_pretty(&snapshots)?);
        }
        Command::TournamentPoll {
            profile,
            scan_megabytes,
        } => {
            let limit = scan_limit_bytes(scan_megabytes)?;
            let mut session = TournamentSession::new(&profile, pointer_width, limit)?;
            // First poll initializes and caches patterns
            let _ = session.poll()?;
            // Second poll demonstrates steady-state cached reading speed
            let snapshot = session.poll()?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            eprintln!(
                "\nSteady-state poll latency: {} µs ({:.3} ms)",
                snapshot.poll_duration_us,
                snapshot.poll_duration_us as f64 / 1000.0
            );
        }
        Command::TournamentWatch {
            profile,
            interval_ms,
            count,
        } => {
            let limit = scan_limit_bytes(128)?;
            let mut session = TournamentSession::new(&profile, pointer_width, limit)?;
            println!("Initializing session and scanning patterns once...");
            let init_snapshot = session.poll()?;
            println!(
                "Attached {} clients (Manager: {}) in {:.2} ms",
                init_snapshot.clients.len(),
                init_snapshot.manager.is_some(),
                init_snapshot.poll_duration_us as f64 / 1000.0
            );

            let mut latencies_us = Vec::new();
            println!(
                "\nStreaming {} polls at {} ms intervals:",
                count, interval_ms
            );
            println!(
                "{:<6} {:<10} {:<12} {:<12} {:<10}",
                "ITER", "LATENCY", "LEFT SCORE", "RIGHT SCORE", "CLIENTS"
            );
            for i in 1..=count {
                std::thread::sleep(Duration::from_millis(interval_ms));
                let snap = session.poll()?;
                latencies_us.push(snap.poll_duration_us);
                let (left_score, right_score) = snap
                    .manager
                    .as_ref()
                    .map(|m| (m.left_score, m.right_score))
                    .unwrap_or((0, 0));
                println!(
                    "{:<6} {:>6.2} ms {:>12} {:>12} {:>10}",
                    i,
                    snap.poll_duration_us as f64 / 1000.0,
                    left_score,
                    right_score,
                    snap.clients.len()
                );
            }
            let avg_us: u128 =
                latencies_us.iter().sum::<u128>() / latencies_us.len().max(1) as u128;
            println!(
                "\nAverage poll latency over {} cycles: {} µs ({:.3} ms)",
                count,
                avg_us,
                avg_us as f64 / 1000.0
            );
        }
        Command::CompareTosu { url } => {
            run_compare_tosu(&url, pointer_width)?;
        }
        Command::Scan {
            pid,
            pattern,
            module,
            max_matches,
            max_megabytes,
        } => {
            let pattern = BytePattern::parse(&pattern)?;
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
            let range = if let Some(module) = module {
                let modules = list_modules(pid)?;
                let module = module_or_main(&modules, &module)?;
                Some((module.base, module.size))
            } else {
                None
            };
            let max_bytes = max_megabytes
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("scan size is too large"))?;
            let matches = memory.scan_pattern(&pattern, range, max_matches, max_bytes)?;
            println!("pattern={} matches={}", pattern.source(), matches.len());
            for address in matches {
                println!("{}", format_address(address));
            }
        }
        Command::ScanProfile {
            pid,
            profile: profile_name,
            key,
            module,
            max_matches,
            max_megabytes,
        } => {
            let profile = load_profile(&profile_name)?;
            let (pattern_source, pattern_offset) = profile.pattern(&key)?;
            let pattern = BytePattern::parse(pattern_source)?;
            let width =
                pointer_width.or((profile.pointer_width > 0).then_some(profile.pointer_width));
            let memory = ProcessMemory::open_with_pointer_size(pid, width)?;
            let range = if let Some(module) = module {
                let modules = list_modules(pid)?;
                let module = module_or_main(&modules, &module)?;
                Some((module.base, module.size))
            } else {
                None
            };
            let max_bytes = max_megabytes
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("scan size is too large"))?;
            let matches = memory.scan_pattern(&pattern, range, max_matches, max_bytes)?;
            let adjusted = matches
                .into_iter()
                .map(|address| apply_pattern_offset(address, pattern_offset))
                .collect::<Result<Vec<_>>>()?;
            println!(
                "profile={} key={} pattern={} offset={} matches={}",
                profile.id,
                key,
                pattern_source,
                pattern_offset,
                adjusted.len()
            );
            for address in adjusted {
                println!("{}", format_address(address));
            }
        }
        Command::TournamentState {
            pid,
            ruleset_address,
        } => {
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width.or(Some(4)))?;
            let state = read_tournament_state(&memory, parse_address(&ruleset_address)?)?;
            println!("{}", serde_json::to_string_pretty(&state)?);
        }
        Command::ResolveRuleset {
            pid,
            profile: profile_name,
        } => {
            let profile = load_profile(&profile_name)?;
            let (pattern_source, pattern_offset) = profile.pattern("rulesets_addr")?;
            let pattern = BytePattern::parse(pattern_source)?;
            let width =
                pointer_width.or((profile.pointer_width > 0).then_some(profile.pointer_width));
            let memory = ProcessMemory::open_with_pointer_size(pid, width)?;
            let matches = memory.scan_pattern(&pattern, None, 1, 512 * 1024 * 1024)?;
            let match_address = matches
                .first()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("rulesets_addr pattern was not found"))?;
            let match_address = apply_pattern_offset(match_address, pattern_offset)?;
            let container_address = match_address
                .checked_sub(0xb)
                .ok_or_else(|| anyhow::anyhow!("ruleset pattern address underflow"))?;
            let container_address = memory
                .read_pointer(container_address)
                .context("reading ruleset container pointer")?;
            let ruleset_address = checked_add_signed(container_address, 4)?;
            let ruleset_address = memory
                .read_pointer(ruleset_address)
                .context("reading ruleset pointer")?;
            println!("{}", format_address(ruleset_address));
        }
        Command::Watch {
            pid,
            address,
            kind,
            interval_ms,
        } => {
            if interval_ms < 10 {
                bail!("interval must be at least 10ms");
            }
            let memory = ProcessMemory::open_with_pointer_size(pid, pointer_width)?;
            let address = parse_address(&address)?;
            loop {
                let value = match kind {
                    ValueKind::U8 => memory.read_u8(address)?.to_string(),
                    ValueKind::U32 => memory.read_u32(address)?.to_string(),
                    ValueKind::I32 => memory.read_i32(address)?.to_string(),
                    ValueKind::U64 => format!("0x{:016X}", memory.read_u64(address)?),
                    ValueKind::F32 => memory.read_f32(address)?.to_string(),
                    ValueKind::F64 => memory.read_f64(address)?.to_string(),
                };
                println!("{}={}", format_address(address), value);
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }
        Command::Profile { name } => {
            if name.eq_ignore_ascii_case("all") {
                for profile in available_profiles() {
                    println!("{profile}");
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&load_profile(&name)?)?);
            }
        }
        #[cfg(feature = "rosu-mem")]
        Command::RosuMem {
            process,
            address,
            length,
        } => {
            let value =
                rtosu_dataprovider::rosu_mem::read_bytes(&process, parse_address(&address)?, length)?;
            print_bytes(&value);
        }
        #[cfg(feature = "rosu-mem")]
        Command::RosuMemSignature { process, pattern } => {
            let address = rtosu_dataprovider::rosu_mem::find_signature(&process, &pattern)?;
            println!("{}", format_address(address));
        }
        Command::Config { action } => {
            let active_path = custom_config_path.unwrap_or(DEFAULT_CONFIG_FILE);
            match action.unwrap_or(ConfigAction::Show) {
                ConfigAction::Show => {
                    println!("Active configuration (from '{}'):\n", active_path);
                    let toml_str = toml::to_string_pretty(&config)?;
                    println!("{toml_str}");
                }
                ConfigAction::Init { path } => {
                    config.save_default_template(&path)?;
                    println!("Created documented configuration template at: {path}");
                }
                ConfigAction::Validate => {
                    config.validate()?;
                    println!(
                        "Configuration at '{}' is valid and within safe limits!",
                        active_path
                    );
                }
            }
        }
        Command::Serve {
            host,
            port,
            poll_rate,
        } => {
            let host = host.unwrap_or(config.server.host.clone());
            let port = port.unwrap_or(config.server.port);
            let poll_hz = poll_rate.unwrap_or(config.poll.poll_rate_hz as u64);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(run_serve_loop(&host, port, poll_hz, pointer_width, config))?;
        }
    }
    Ok(())
}

async fn run_serve_loop(
    host: &str,
    port: u16,
    poll_rate_hz: u64,
    pointer_width: Option<usize>,
    config: AppConfig,
) -> Result<()> {
    let (tx, rx) = tokio::sync::watch::channel(rtosu_dataprovider::v2::TosuV2Packet::default());
    let enable_http = config.server.enable_http;
    let enable_ws = config.server.enable_websocket;
    let cors_allow_all = config.server.cors_allow_all;

    let listener = if enable_http || enable_ws {
        match rtosu_dataprovider::server::bind_listener(host, port).await {
            Ok(listener) => Some(listener),
            Err(err) => {
                let err_msg = format!("{err:#}");
                eprintln!("===========================================================");
                eprintln!(" [FATAL] Server startup failed: http://{}:{}", host, port);
                eprintln!("===========================================================");
                if err_msg.contains("os error 10048")
                    || err_msg.contains("Address already in use")
                    || err_msg.contains("address already in use")
                {
                    eprintln!(" Port {} is already in use by another application!", port);
                    eprintln!(" (e.g. tosu, another running instance of rtosu, or another web server)");
                    eprintln!();
                    eprintln!(" How to fix:");
                    eprintln!("   1. Close conflicting instances (tosu / rtosu-dataprovider.exe).");
                    eprintln!("   2. Or run on a different port: rtosu-dataprovider.exe serve --port 24051");
                    eprintln!("   3. Or change port = 24051 under [server] in config.toml");
                } else {
                    eprintln!(" Reason: {}", err_msg);
                }
                eprintln!("===========================================================");
                tracing::error!("Failed to bind server listener to {}:{}: {:#}", host, port, err);
                return Err(err);
            }
        }
    } else {
        None
    };

    if let Some(listener) = listener {
        tokio::spawn(async move {
            if let Err(e) = rtosu_dataprovider::server::serve_with_listener(
                listener,
                enable_http,
                enable_ws,
                cors_allow_all,
                rx,
            )
            .await
            {
                tracing::error!("tosu Server error: {e:#}");
            }
        });
    } else {
        tracing::info!(
            "Server feature toggles: HTTP and WebSocket are both disabled. Zero-port bypass active; skipping server spawn."
        );
    }

    println!("===========================================================");
    if enable_http || enable_ws {
        let display_host = if host == "0.0.0.0" { "127.0.0.1" } else { host };
        println!(
            " tosu Rust Native Replacement Server running on http://{}:{}",
            host, port
        );
        if host == "0.0.0.0" {
            println!(" (Bound to 0.0.0.0:{} - accessible locally and via your LAN IP)", port);
        }
        println!(" Live Endpoints:");
        if enable_http {
            println!("   - HTTP JSON:        http://{}:{}/json/v2", display_host, port);
            println!("   - Health check:     http://{}:{}/health", display_host, port);
        }
        if enable_ws {
            println!("   - WebSocket Stream: ws://{}:{}/websocket/v2", display_host, port);
        }
    } else {
        println!(" tosu Rust Native Data Provider running in headless reader mode");
        println!(" (Zero-port bypass active: HTTP and WebSocket servers are disabled)");
    }
    println!(
        " Polling rate: {} Hz ({} ms interval)",
        poll_rate_hz,
        1000 / poll_rate_hz.max(1)
    );

    let mode = if config.poll.auto_mode {
        rtosu_dataprovider::OsuReaderMode::Auto
    } else if config.poll.default_profile.eq_ignore_ascii_case("tournament") {
        rtosu_dataprovider::OsuReaderMode::Tournament
    } else {
        rtosu_dataprovider::OsuReaderMode::Solo
    };

    println!(" Mode: {}", match mode {
        rtosu_dataprovider::OsuReaderMode::Auto => "Auto-detection (Single-Player & Tournament)",
        rtosu_dataprovider::OsuReaderMode::Tournament => "Locked to Tournament Mode",
        rtosu_dataprovider::OsuReaderMode::Solo => "Locked to Single-Player Mode",
    });
    println!(
        " Features: pp_calc={}, chat_attribution={}",
        config.features.enable_pp,
        config.features.enable_chat
    );
    println!("===========================================================");

    let interval = Duration::from_millis(1000 / poll_rate_hz.max(1));
    let limit = (config.poll.scan_budget_mb * 1024 * 1024) as usize;
    let mut reader = rtosu_dataprovider::OsuReader::builder()
        .tournament_profile(&config.poll.default_profile)
        .solo_profile("stable")
        .mode(mode)
        .enable_pp(config.features.enable_pp)
        .enable_chat(config.features.enable_chat)
        .opt_pointer_width(pointer_width)
        .scan_limit_bytes(limit)
        .poll_interval(interval)
        .build()?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received shutdown signal (Ctrl+C). Terminating cleanly...");
                println!("\nShutdown signal received. Exiting rtosu-dataprovider... Goodbye!");
                break;
            }
            _ = tokio::time::sleep(interval) => {
                if let Ok(packet) = reader.poll() {
                    let _ = tx.send(packet);
                }
            }
        }
    }

    Ok(())
}

fn http_get_localhost(port: u16, path: &str) -> Result<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("connecting to localhost:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let response_str = String::from_utf8_lossy(&response);
    if let Some(pos) = response_str.find("\r\n\r\n") {
        Ok(response_str[pos + 4..].to_string())
    } else {
        Ok(response_str.to_string())
    }
}

fn run_compare_tosu(url: &str, pointer_width: Option<usize>) -> Result<()> {
    println!("Fetching tosu live tournament data from {url}...");
    let tosu_raw = if url.contains("24050") {
        http_get_localhost(24050, "/json/v2")?
    } else {
        bail!("Only localhost:24050 is supported for direct comparison");
    };
    let tosu_val: serde_json::Value =
        serde_json::from_str(&tosu_raw).context("parsing tosu json")?;
    let tosu_tourney = tosu_val
        .get("tourney")
        .ok_or_else(|| anyhow::anyhow!("tosu json missing 'tourney' field"))?;

    println!("Initializing native Rust TournamentSession...");
    let limit = scan_limit_bytes(128)?;
    let mut session = TournamentSession::new("tournament", pointer_width, limit)?;
    // Warmup scan
    let _ = session.poll()?;
    // Steady state poll
    let rust_snap = session.poll()?;

    println!("\n=== PERFORMANCE ===");
    println!(
        "Native Rust Steady-State Poll: {} µs ({:.3} ms)",
        rust_snap.poll_duration_us,
        rust_snap.poll_duration_us as f64 / 1000.0
    );

    println!("\n=== TOURNAMENT MANAGER COMPARISON ===");
    let tosu_ipc_state = tosu_tourney
        .get("ipcState")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    let tosu_best_of = tosu_tourney
        .get("bestOF")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    let tosu_stars_left = tosu_tourney
        .get("points")
        .and_then(|v| v.get("left"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let tosu_stars_right = tosu_tourney
        .get("points")
        .and_then(|v| v.get("right"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let tosu_score_left = tosu_tourney
        .get("totalScore")
        .and_then(|v| v.get("left"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let tosu_score_right = tosu_tourney
        .get("totalScore")
        .and_then(|v| v.get("right"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let tosu_team_left = tosu_tourney
        .get("team")
        .and_then(|v| v.get("left"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tosu_team_right = tosu_tourney
        .get("team")
        .and_then(|v| v.get("right"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if let Some(ref m) = rust_snap.manager {
        println!(
            "{:<20} | {:<20} | {:<20} | Status",
            "Field", "tosu", "Rust Native"
        );
        println!("{:-<20}-+-{:-<20}-+-{:-<20}-+-------", "", "", "");
        print_cmp(
            "ipcState",
            &tosu_ipc_state.to_string(),
            &m.ipc_state.to_string(),
        );
        print_cmp("bestOF", &tosu_best_of.to_string(), &m.best_of.to_string());
        print_cmp(
            "points.left",
            &tosu_stars_left.to_string(),
            &m.left_stars.to_string(),
        );
        print_cmp(
            "points.right",
            &tosu_stars_right.to_string(),
            &m.right_stars.to_string(),
        );
        print_cmp(
            "score.left",
            &tosu_score_left.to_string(),
            &m.left_score.to_string(),
        );
        print_cmp(
            "score.right",
            &tosu_score_right.to_string(),
            &m.right_score.to_string(),
        );
        print_cmp("team.left", tosu_team_left, &m.first_team_name);
        print_cmp("team.right", tosu_team_right, &m.second_team_name);
    } else {
        println!("Rust could not find manager process!");
    }

    println!("\n=== SPECTATOR CLIENTS COMPARISON ===");
    let tosu_clients = tosu_tourney.get("clients").and_then(|v| v.as_array());
    println!(
        "{:<5} {:<6} {:<16} {:<12} {:<8} {:<8} {:<8}",
        "ipcId", "TEAM", "PLAYER NAME", "SCORE", "ACCURACY", "COMBO", "MODS"
    );
    println!(
        "{:-<5} {:-<6} {:-<16} {:-<12} {:-<8} {:-<8} {:-<8}",
        "", "", "", "", "", "", ""
    );

    for rust_client in &rust_snap.clients {
        let rust_name = rust_client
            .user
            .as_ref()
            .map(|u| u.name.as_str())
            .unwrap_or("?");
        let rust_score = rust_client.gameplay.as_ref().map(|g| g.score).unwrap_or(0);
        let rust_acc = rust_client
            .gameplay
            .as_ref()
            .map(|g| g.accuracy)
            .unwrap_or(0.0);
        let rust_combo = rust_client.gameplay.as_ref().map(|g| g.combo).unwrap_or(0);
        let rust_mods = rust_client
            .gameplay
            .as_ref()
            .map(|g| g.mods_str.as_str())
            .unwrap_or("");

        // Find corresponding tosu client
        let tosu_client = tosu_clients.and_then(|arr| {
            arr.iter().find(|c| {
                c.get("ipcId").and_then(|v| v.as_u64()) == Some(rust_client.ipc_id as u64)
            })
        });

        let (t_team, t_name, t_score, t_acc, t_combo, t_mods) = if let Some(tc) = tosu_client {
            (
                tc.get("team").and_then(|v| v.as_str()).unwrap_or(""),
                tc.get("user")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                tc.get("play")
                    .and_then(|v| v.get("score"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                tc.get("play")
                    .and_then(|v| v.get("accuracy"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                tc.get("play")
                    .and_then(|v| v.get("combo"))
                    .and_then(|v| v.get("current"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                tc.get("play")
                    .and_then(|v| v.get("mods"))
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
        } else {
            ("", "", 0, 0.0, 0, "")
        };

        println!(
            "Rust [#{}]  {:<6} {:<16} {:<12} {:<8.2}% {:<8} {:<8}",
            rust_client.ipc_id,
            rust_client.team,
            rust_name,
            rust_score,
            rust_acc,
            rust_combo,
            rust_mods
        );
        println!(
            "tosu [#{}]  {:<6} {:<16} {:<12} {:<8.2}% {:<8} {:<8}",
            rust_client.ipc_id, t_team, t_name, t_score, t_acc, t_combo, t_mods
        );
        println!("{:-<75}", "");
    }

    Ok(())
}

fn print_cmp(field: &str, tosu: &str, rust: &str) {
    let matches = tosu == rust;
    let status = if matches { "MATCH" } else { "DIFF" };
    println!("{:<20} | {:<20} | {:<20} | {}", field, tosu, rust, status);
}

fn scan_limit_bytes(megabytes: usize) -> Result<usize> {
    megabytes
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("scan size is too large"))
}

fn apply_pattern_offset(address: u64, offset: i64) -> Result<u64> {
    checked_add_signed(address, offset)
}

fn parse_pointer_width(value: &str) -> Result<usize, String> {
    match value {
        "4" => Ok(4),
        "8" => Ok(8),
        _ => Err("pointer width must be 4 or 8".to_owned()),
    }
}

fn parse_address(value: &str) -> Result<u64> {
    parse_u128_as_u64(value)
}

fn resolve_read_address(pid: u32, address: &str, module: Option<&str>) -> Result<u64> {
    match module {
        Some(module) => {
            let modules = list_modules(pid)?;
            let module = module_or_main(&modules, module)?;
            checked_add(module.base, parse_u64(address)?)
        }
        None => parse_address(address),
    }
}

fn print_bytes(bytes: &[u8]) {
    for chunk in bytes.chunks(16) {
        let hex = chunk
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("{hex}");
    }
}
