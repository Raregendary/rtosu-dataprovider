//! Hermetic microbenchmarks for the CPU-bound parts of the data provider.
//!
//! These do not touch a live osu! process, so they can run anywhere and on CI.
//! Their purpose is to quantify the per-call costs that the 60 Hz poll loop
//! multiplies by 60, and to give a regression baseline for the pattern matcher,
//! address parsing, mod handling and JSON serialization.
//!
//! Run with:
//! ```powershell
//! cargo bench --bench micro
//! cargo bench --bench micro -- --save-baseline before
//! cargo bench --bench micro -- --baseline before
//! ```
//!
//! Caveats recorded in Benchmarks/report.md:
//! * `BytePattern::find_all` is deliberately benchmarked on synthetic buffers.
//!   The live scan additionally pays `ReadProcessMemory` per chunk, so these
//!   numbers are a lower bound on real scan cost.
//! * PP benchmarks use distinct map ids per iteration so the process-global
//!   chunk cache does not turn the measurement into a cache-hit benchmark.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use rtosu_dataprovider::address::{format_address, parse_i64, parse_u64, parse_u128_as_u64};
use rtosu_dataprovider::beatmap::BeatmapSnapshot;
use rtosu_dataprovider::client::{
    calculate_accuracy, calculate_tosu_grade, calculate_unstable_rate, format_mods,
    hit_error_window, mod_acronyms, parse_hit_errors,
};
use rtosu_dataprovider::pattern::BytePattern;
use rtosu_dataprovider::v2::{GraphSeries, PlayState, TosuV2Packet, create_mods_state};
use std::hint::black_box;
#[cfg(feature = "pp")]
use std::time::{Duration, Instant};

// A pattern taken from the shipped stable profile: mixed concrete bytes and
// wildcards, i.e. the realistic scan workload.
const REAL_PATTERN: &str = "8B 0D ?? ?? ?? ?? 85 C9 74 ?? 8B 0D ?? ?? ?? ?? 8B 01 5A C3";
// Worst case for a naive matcher: first byte never occurs in the buffer.
const MISSING_ANCHOR_PATTERN: &str = "FF 0F 1E ?? ?? 66 2E 0F";
// Best case: an anchor byte that is unique in the buffer.
const UNIQUE_ANCHOR_PATTERN: &str = "7F 4C ?? ?? 66 90";

fn filler(len: usize, seed: u64) -> Vec<u8> {
    // xorshift64* so the buffer is not compressible in a way that changes the
    // branch behaviour of the matcher.
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
        })
        .collect()
}

/// Hit errors are `i16` since the storage change: the same numbers on the wire,
/// half the bytes in RAM, and half the bytes to decode.
fn representative_hit_errors(n: usize) -> Vec<i16> {
    // Human-ish distribution: mostly small values with a few outliers, which is
    // what read_hit_errors produces mid-map.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as i32 % 41) as i16
        })
        .collect()
}

fn representative_packet(hit_errors: usize) -> TosuV2Packet {
    let mut packet = TosuV2Packet {
        client: "stable".to_string(),
        server: "ppy.sh".to_string(),
        ..Default::default()
    };
    packet.state.name = "play".to_string();
    packet.state.number = 3;
    packet.beatmap.artist = "DJ Sharpnel".to_string();
    packet.beatmap.title = "WE LUV LAMA 3".to_string();
    packet.beatmap.version = "WE LUV RSI (54465)".to_string();
    packet.beatmap.checksum = "0123456789abcdef0123456789abcdef".to_string();
    packet.beatmap.time.live = 629_082;
    packet.beatmap.time.last_object = 3_890_905;
    packet.beatmap.time.mp3_length = 3_895_928;
    packet.beatmap.stats.bpm.common = 240.0;
    packet.beatmap.stats.objects.total = 8817;
    packet.play = PlayState {
        player_name: "osu!".to_string(),
        score: 668_490,
        accuracy: 99.87,
        combo: crate_combo(8817),
        hit_error_array: representative_hit_errors(hit_errors).into(),
        mods: create_mods_state(536_872_960, "ATv2"),
        rank: crate_rank("SS", "SS"),
        unstable_rate: 3.42,
        ..Default::default()
    };
    packet
}

fn crate_combo(v: i32) -> rtosu_dataprovider::v2::ComboState {
    rtosu_dataprovider::v2::ComboState { current: v, max: v }
}

/// A packet that looks like the real thing during a marathon map: the strain
/// graph dominates the payload (measured live: 552 KB of 556 KB for a
/// 9 740-point graph over 5 series), so it dominates serialization cost too.
fn marathon_packet(points: usize, series: usize) -> TosuV2Packet {
    let mut packet = representative_packet(0);
    let xaxis = (0..points)
        .map(|i| (i as i64) * 400 + 48)
        .map(|v| v as i32)
        .collect();
    let mut graph = rtosu_dataprovider::v2::PerformanceGraph {
        series: Vec::new(),
        xaxis,
    };
    for s in 0..series {
        graph.series.push(GraphSeries {
            name: ["aim", "aimNoSliders", "reading", "flashlight", "speed"][s % 5].to_string(),
            data: (0..points).map(|i| (i % 977) as f32 * 0.05).collect(),
        });
    }
    packet.performance.graph = rtosu_dataprovider::v2::PrecomputedGraph::new(&graph);
    packet
}

fn crate_rank(cur: &str, max: &str) -> rtosu_dataprovider::v2::RankState {
    rtosu_dataprovider::v2::RankState {
        current: cur.to_string(),
        max_this_play: max.to_string(),
    }
}

fn bench_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("pattern");
    let missing = BytePattern::parse(MISSING_ANCHOR_PATTERN).expect("parse missing pattern");
    let unique = BytePattern::parse(UNIQUE_ANCHOR_PATTERN).expect("parse unique pattern");

    group.throughput(Throughput::Bytes(1024));
    group.bench_function("parse_real_pattern", |b| {
        b.iter(|| BytePattern::parse(black_box(REAL_PATTERN)).unwrap().len())
    });

    let no_match = filler(1024 * 1024, 0x1234_5678);
    let mut early = filler(1024 * 1024, 0x1234_5678);
    let head = BytePattern::parse(REAL_PATTERN).unwrap();
    // Place a guaranteed match by copying the pattern's concrete bytes.
    let concrete: Vec<u8> = REAL_PATTERN
        .split_whitespace()
        .map(|t| u8::from_str_radix(t, 16).unwrap_or(0))
        .collect();
    for (i, byte) in concrete.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        early[i] = *byte;
    }
    assert!(head.matches_at(&early, 0));

    group.throughput(Throughput::Bytes(1024 * 1024));
    group.bench_function("scan_1mib_worst_case_no_match", |b| {
        b.iter(|| {
            black_box(
                missing
                    .find_all(black_box(&no_match), 0x1000_0000, 16)
                    .len(),
            )
        })
    });
    group.bench_function("scan_1mib_match_at_start", |b| {
        b.iter(|| {
            black_box(early.len())
                + black_box(head.find_all(black_box(&early), 0x1000_0000, 1).len())
        })
    });
    group.bench_function("scan_1mib_unique_anchor", |b| {
        b.iter(|| black_box(unique.find_all(black_box(&no_match), 0x1000_0000, 16).len()))
    });
    group.finish();
}

fn bench_address(c: &mut Criterion) {
    let mut group = c.benchmark_group("address");
    group.bench_function("parse_u64_hex", |b| {
        b.iter(|| parse_u64(black_box("0x000000001A2B3C4D")).unwrap())
    });
    group.bench_function("parse_u64_dec", |b| {
        b.iter(|| parse_u64(black_box("4402341965")).unwrap())
    });
    group.bench_function("parse_u128_as_u64", |b| {
        b.iter(|| parse_u128_as_u64(black_box("0x0000000005F5E100")).unwrap())
    });
    group.bench_function("parse_i64_signed", |b| {
        b.iter(|| parse_i64(black_box("-0x1F4")).unwrap())
    });
    group.bench_function("format_address", |b| {
        b.iter(|| format_address(black_box(0x05F5_E100u64)).len())
    });
    group.finish();
}

fn bench_mods(c: &mut Criterion) {
    const MODS: u32 = 536_872_960; // ATv2
    let mut group = c.benchmark_group("mods");
    group.bench_function("format_mods", |b| {
        b.iter(|| format_mods(black_box(MODS)).len())
    });
    group.bench_function("mod_acronyms", |b| {
        b.iter(|| mod_acronyms(black_box(MODS)).len())
    });
    group.bench_function("create_mods_state", |b| {
        b.iter(|| {
            create_mods_state(black_box(MODS), black_box("ATv2"))
                .checksum
                .len()
        })
    });
    group.finish();
}

fn bench_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("scoring");
    group.bench_function("calculate_accuracy", |b| {
        b.iter(|| {
            calculate_accuracy(
                black_box(0),
                black_box(8802),
                black_box(0),
                black_box(0),
                black_box(0),
            )
        })
    });
    group.bench_function("calculate_tosu_grade", |b| {
        b.iter(|| {
            calculate_tosu_grade(
                black_box(0),
                black_box(99.87),
                black_box(8802),
                black_box(10),
                black_box(1),
                black_box(0),
                black_box(MODS_FULL),
            )
            .len()
        })
    });
    for n in [100usize, 1000, 10_000] {
        let errors = representative_hit_errors(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("calculate_unstable_rate_{n}"), |b| {
            b.iter(|| calculate_unstable_rate(black_box(&errors), black_box(MODS_FULL)))
        });
    }
    group.finish();
}

const MODS_FULL: u32 = 1 << 15; // representative mod bitfield

/// The pure decode half of `read_hit_errors`: the syscall is excluded, so this
/// is a lower bound on the real cost, but it is the part that scales with the
/// array length and the part the old per-element loop paid 6 230 times.
///
/// Two details matter and both were wrong at some point:
///
/// * osu! stores hit errors in a .NET `List<int>`, so the wire format is **four
///   bytes per element** even though the decoded type is now `i16`. Packing the
///   benchmark's `i16` values with `i16::to_le_bytes` produces two bytes per
///   element, which `chunks_exact(4)` then reads as pairs — the benchmark would
///   measure the wrong function on the wrong data.
/// * the decoded elements have to escape. Returning only `.len()` lets LLVM fold
///   the loop to a length computation and drop the element stores, which showed
///   up as a flat 0.029 us for 100, 1 000, 6 230 *and* 16 000 elements.
///   `black_box(&decoded)` forces the allocation and its contents to be
///   observable.
fn bench_hit_error_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("hit_errors");
    for n in [100usize, 1_000, 6_230, 16_000, 20_000] {
        let values = representative_hit_errors(n);
        // Four bytes per element, the layout `read_hit_errors` actually reads.
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|v| (*v as i32).to_le_bytes())
            .collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("bulk_decode_{n}"), |b| {
            b.iter(|| {
                let decoded = parse_hit_errors(black_box(&bytes));
                let len = decoded.len();
                black_box(&decoded);
                len
            })
        });
        group.bench_function(format!("hit_error_window_{n}"), |b| {
            b.iter(|| {
                let (start, count) = hit_error_window(black_box(n));
                black_box(start) + black_box(count)
            })
        });
    }
    group.finish();
}

fn bench_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialize");
    for n in [0usize, 100, 1000, 10_000] {
        let packet = representative_packet(n);
        let json = serde_json::to_string(&packet).expect("serialize");
        group.throughput(Throughput::Bytes(json.len() as u64));
        group.bench_function(format!("packet_to_json_hiterrors_{n}"), |b| {
            b.iter(|| serde_json::to_string(black_box(&packet)).unwrap().len())
        });
    }
    let packet = representative_packet(1000);
    let cloned = packet.clone();
    group.bench_function("packet_clone_hiterrors_1000", |b| {
        b.iter(|| black_box(cloned.clone()).play.score)
    });
    group.finish();

    // The real payload during a long map is dominated by the strain graph.
    let mut graph_group = c.benchmark_group("serialize_graph");
    for (points, series) in [(9740usize, 5usize), (2500, 5), (2500, 1)] {
        let packet = marathon_packet(points, series);
        let json = serde_json::to_string(&packet).expect("serialize graph packet");
        graph_group.throughput(Throughput::Bytes(json.len() as u64));
        graph_group.bench_function(format!("to_json_{points}x{series}"), |b| {
            b.iter(|| serde_json::to_string(black_box(&packet)).unwrap().len())
        });
    }
    let marathon = marathon_packet(9740, 5);
    graph_group.throughput(Throughput::Elements(9740 * 5));
    graph_group.bench_function("clone_9740x5", |b| {
        b.iter(|| black_box(marathon.clone()).play.score)
    });
    graph_group.finish();
}

fn bench_profile_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("profile");
    let path = std::path::Path::new("profiles/stable.json");
    group.bench_function("load_profile_file_stable", |b| {
        b.iter(|| {
            rtosu_dataprovider::profile::load_profile_file(black_box(path))
                .unwrap()
                .patterns
                .len()
        })
    });
    group.bench_function("load_profile_embedded", |b| {
        b.iter(|| {
            rtosu_dataprovider::profile::load_profile(black_box("tournament"))
                .unwrap()
                .patterns
                .len()
        })
    });
    group.finish();
}

fn bench_beatmap_metadata(c: &mut Criterion) {
    // A synthetic .osu file written once, so the benchmark measures the parser
    // and not the disk.
    let dir = std::env::temp_dir().join("rtosu-bench-beatmap");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("bench [bpm].osu");
    let mut body =
        String::from("osu file format v14\n\n[General]\nAudioFilename: audio.mp3\nMode: 0\n");
    body.push_str("[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.8\nSliderTickRate:1\nAudioLeadIn:0\n");
    for i in 0..2000 {
        body.push_str(&format!("{i},100,{},0,1,0\n", 100 + (i % 7) * 3));
    }
    std::fs::write(&path, body).expect("write synthetic .osu");

    let mut group = c.benchmark_group("beatmap_metadata");
    group.bench_function("populate_beatmap_file_metadata_2000_objects", |b| {
        b.iter_batched(
            BeatmapSnapshot::default,
            |mut snapshot| {
                rtosu_dataprovider::beatmap::populate_beatmap_file_metadata(&mut snapshot, &path);
                black_box(snapshot.stats.objects.total)
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

/// Synthetic osu!std map with `objects` circles at a fixed 300 ms spacing.
///
/// Long enough that the difficulty calculation is not dominated by the
/// per-beatmap constant, short enough to build a 30 000-object body quickly.
#[cfg(feature = "pp")]
fn synthetic_osu_map(objects: usize) -> rosu_pp::Beatmap {
    let mut body = String::with_capacity(objects * 24);
    body.push_str(
        "osu file format v14\n\n[General]\nAudioFilename: audio.mp3\nMode: 0\n\n[Metadata]\nTitle:bench\nArtist:bench\nCreator:bench\nVersion:bench\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,333.333333333333,4,2,0,70,1,0\n\n[HitObjects]\n",
    );
    for i in 0..objects {
        // A 3-3-0-7 stream so consecutive objects are not a trivially uniform
        // pattern; uniform spacing would let the strain calculator shortcut.
        let x = 64 + (i * 37) % 384;
        let y = 64 + (i * 53) % 256;
        body.push_str(&format!("{x},{y},{i},1,0,0:0:0:0:\n"));
    }
    rosu_pp::Beatmap::from_bytes(body.as_bytes()).expect("parse synthetic map")
}

/// Gradual-PP chunk scaling: `N = 1` (full map only) against `N = 100`.
///
/// Two things are measured:
///
/// 1. **One-shot, outside criterion**: what the poll loop actually pays on the
///    first tick of a map with `objects` hit objects, and how long the
///    background `GradualDifficulty` worker then takes to replace the fallback.
///    This is the number `report.md` §6.3's 145 s stall has to be compared
///    against, and the live harness cannot produce it because it needs a map
///    big enough for the old synchronous path to be pathological.
/// 2. **criterion**: `compute_chunks` at `N = 1 / 10 / 100 / 250`, which is the
///    pure cost of the computation the worker thread runs.
#[cfg(feature = "pp")]
fn bench_pp_chunks(c: &mut Criterion) {
    use rosu_mods::GameModsLegacy;
    use rtosu_dataprovider::pp::calculator::{compute_chunks, get_or_compute_gradual_chunks};

    let mods = GameModsLegacy::default();

    for (objects, map_id) in [
        (5_000usize, 0x5A00_0001u32),
        (15_000, 0x5B00_0001),
        (30_000, 0x5C00_0001),
    ] {
        let map = synthetic_osu_map(objects);
        let started = Instant::now();
        let first = get_or_compute_gradual_chunks(map_id, &map, mods, 100);
        let first_call_ms = started.elapsed().as_secs_f64() * 1000.0;
        let first_chunks = first.len();

        // Poll the same key until the worker has published the real chunks.
        // This is "time until gradual PP is live", which the cumulative phase
        // counters can only report at process exit.
        let mut waited_ms = first_call_ms;
        let mut chunks = first;
        while chunks.len() <= 1 && waited_ms < 300_000.0 {
            std::thread::sleep(Duration::from_millis(20));
            chunks = get_or_compute_gradual_chunks(map_id, &map, mods, 100);
            waited_ms = started.elapsed().as_secs_f64() * 1000.0;
        }
        println!(
            "pp_first_call objects={objects} first_call_ms={first_call_ms:.1} \
             first_call_chunks={first_chunks} gradual_ready_ms={waited_ms:.1} \
             gradual_chunks={} off_critical_path={:.0}x",
            chunks.len(),
            waited_ms / first_call_ms.max(f64::MIN_POSITIVE),
        );
        // Let the worker finish before the next size so two workers do not
        // compete for the same core and distort both numbers.
        std::thread::sleep(Duration::from_millis(250));
    }

    let map = synthetic_osu_map(5_000);
    let mut group = c.benchmark_group("pp_chunks");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));
    group.throughput(Throughput::Elements(5_000));
    for n in [1usize, 10, 100, 250] {
        group.bench_function(format!("n{n}_5000"), |b| {
            b.iter(|| black_box(compute_chunks(black_box(&map), mods, n)));
        });
    }
    group.finish();
}

#[cfg(not(feature = "pp"))]
fn bench_pp_chunks(_c: &mut Criterion) {}

criterion_group!(
    benches,
    bench_pattern,
    bench_address,
    bench_mods,
    bench_scoring,
    bench_hit_error_decode,
    bench_serialize,
    bench_profile_parse,
    bench_beatmap_metadata,
    bench_pp_chunks,
);
criterion_main!(benches);
