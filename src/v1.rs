//! The gosumemory-compatible payload, served at `/json`, `/json/v1` and on the
//! `/ws` socket.
//!
//! tosu exposes this as `GosuCompatibleApi` on `/json` and on `/ws`
//! (`tosu-sourcecode/packages/server/router/index.ts:43`,
//! `router/socket.ts:47`), and rtosu serves it on the same two paths. `/json/v1`
//! is an extra alias for the same payload, added because rtosu used to serve v2
//! on `/json`; v2's own path is `/json/v2` and the two shapes are not
//! interchangeable.
//!
//! Every type here is a pure reshape of the v2 packet, so there is one reader, one
//! cache and one source of truth. The shapes are dictated by
//! `tosu-sourcecode/packages/tosu/src/api/types/v1.ts` and, where the two disagree,
//! by the assembler at `tosu-sourcecode/packages/tosu/src/api/utils/buildResult.ts`.
//!
//! # Key order is part of the contract
//!
//! `JSON.stringify` emits array-index keys first, in ascending numeric order, ahead
//! of every string key (ECMA-262 `OrdinaryOwnPropertyKeys`). So the wire order of
//! `menu.pp`, `gameplay.hits` and `resultsScreen` is **not** the order
//! `buildResult.ts` writes them, and not the order `v1.ts` declares them. The orders
//! below are the ones tosu actually emits, captured from a live response:
//!
//! - `menu.pp`: `90, 91, … 100, strains, strainsAll`
//! - `gameplay.hits`: `0, 50, 100, 300, geki, katu, sliderEndHits, smallTickHits,
//!   largeTickHits, sliderBreaks, grade, unstableRate, hitErrorArray` (13 keys)
//! - `resultsScreen`: `0, 50, 100, 300, mode, name, score, accuracy, maxCombo,
//!   mods, geki, katu, grade, createdAt`
//!
//! Serde emits declared field order, so the integer-like keys are declared first
//! with an explicit `rename`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::v2::TosuV2Packet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GosuCompatibleApi {
    pub client: String,
    pub settings: V1Settings,
    pub menu: V1Menu,
    pub gameplay: V1Gameplay,
    #[serde(rename = "resultsScreen")]
    pub results_screen: V1ResultsScreen,
    #[serde(rename = "userProfile")]
    pub user_profile: V1UserProfile,
    pub tourney: V1Tourney,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Settings {
    #[serde(rename = "showInterface")]
    pub show_interface: bool,
    pub folders: V1Folders,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Folders {
    pub game: String,
    pub skin: String,
    pub songs: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Menu {
    #[serde(rename = "mainMenu")]
    pub main_menu: V1MainMenu,
    pub state: i32,
    #[serde(rename = "gameMode")]
    pub game_mode: i32,
    #[serde(rename = "isChatEnabled")]
    pub is_chat_enabled: u8,
    pub bm: V1MenuBeatmap,
    pub mods: V1Mods,
    pub pp: V1MenuPp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MainMenu {
    #[serde(rename = "bassDensity")]
    pub bass_density: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MenuBeatmap {
    pub time: V1MenuBeatmapTime,
    pub id: i32,
    pub set: i32,
    pub md5: String,
    #[serde(rename = "rankedStatus")]
    pub ranked_status: i32,
    pub metadata: V1Metadata,
    pub stats: V1MenuStats,
    pub path: V1MenuPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MenuBeatmapTime {
    #[serde(rename = "firstObj")]
    pub first_obj: i32,
    pub current: i32,
    pub full: i32,
    pub mp3: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Metadata {
    pub artist: String,
    #[serde(rename = "artistOriginal")]
    pub artist_original: String,
    pub title: String,
    #[serde(rename = "titleOriginal")]
    pub title_original: String,
    pub mapper: String,
    pub difficulty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MenuStats {
    #[serde(rename = "AR")]
    pub ar: f32,
    #[serde(rename = "CS")]
    pub cs: f32,
    #[serde(rename = "OD")]
    pub od: f32,
    #[serde(rename = "HP")]
    pub hp: f32,
    #[serde(rename = "SR")]
    pub sr: f32,
    #[serde(rename = "BPM")]
    pub bpm: V1Bpm,
    pub circles: i32,
    pub sliders: i32,
    pub spinners: i32,
    pub holds: i32,
    #[serde(rename = "maxCombo")]
    pub max_combo: i32,
    #[serde(rename = "fullSR")]
    pub full_sr: f32,
    #[serde(rename = "memoryAR")]
    pub memory_ar: f32,
    #[serde(rename = "memoryCS")]
    pub memory_cs: f32,
    #[serde(rename = "memoryOD")]
    pub memory_od: f32,
    #[serde(rename = "memoryHP")]
    pub memory_hp: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Bpm {
    pub realtime: f32,
    pub common: f32,
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MenuPath {
    /// tosu builds this as `path.join(menu.folder, menu.backgroundFilename)`
    /// (`buildResult.ts:143-146`) -- the **background image**, not the `.osu` file.
    /// The name is misleading and reproducing it is the point.
    pub full: String,
    pub folder: String,
    pub file: String,
    pub bg: String,
    pub audio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Mods {
    pub num: u32,
    pub str: String,
}

/// The 11-step accuracy table, then the two strain blocks.
///
/// Declared in the order `JSON.stringify` emits them: the integer-like keys are
/// hoisted and sorted ahead of `strains` and `strainsAll` regardless of where
/// `buildResult.ts:157-161` puts the spread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1MenuPp {
    #[serde(rename = "90")]
    pub n90: f32,
    #[serde(rename = "91")]
    pub n91: f32,
    #[serde(rename = "92")]
    pub n92: f32,
    #[serde(rename = "93")]
    pub n93: f32,
    #[serde(rename = "94")]
    pub n94: f32,
    #[serde(rename = "95")]
    pub n95: f32,
    #[serde(rename = "96")]
    pub n96: f32,
    #[serde(rename = "97")]
    pub n97: f32,
    #[serde(rename = "98")]
    pub n98: f32,
    #[serde(rename = "99")]
    pub n99: f32,
    #[serde(rename = "100")]
    pub n100: f32,
    /// The mode's primary skill series, zero padded. tosu's `beatmapPP.strains`;
    /// for osu!std that is the aim series.
    pub strains: Vec<f64>,
    #[serde(rename = "strainsAll")]
    pub strains_all: V1StrainsAll,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1StrainsAll {
    pub series: Vec<crate::v2::GraphSeries>,
    pub xaxis: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Gameplay {
    #[serde(rename = "gameMode")]
    pub game_mode: i32,
    pub name: String,
    pub score: i32,
    pub accuracy: f64,
    pub combo: crate::v2::ComboState,
    pub hp: crate::v2::HealthBarState,
    pub hits: V1GameplayHits,
    pub pp: V1GameplayPp,
    #[serde(rename = "keyOverlay")]
    pub key_overlay: V1KeyOverlay,
    pub leaderboard: V1Leaderboard,
    #[serde(rename = "_isReplayUiHidden")]
    pub is_replay_ui_hidden: bool,
}

/// Thirteen keys, not the six `v1.ts:171-182` declares.
///
/// `toLegacyHits` (`utils/hitResult.ts:40-51`) returns nine, and the builder adds
/// `sliderBreaks`, `grade`, `unstableRate` and `hitErrorArray` on top. The three
/// tick keys are absent from the declared interface but present on the wire, so a
/// build against the schema alone silently drops them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1GameplayHits {
    #[serde(rename = "0")]
    pub n0: i32,
    #[serde(rename = "50")]
    pub n50: i32,
    #[serde(rename = "100")]
    pub n100: i32,
    #[serde(rename = "300")]
    pub n300: i32,
    pub geki: i32,
    pub katu: i32,
    #[serde(rename = "sliderEndHits")]
    pub slider_end_hits: i32,
    #[serde(rename = "smallTickHits")]
    pub small_tick_hits: i32,
    #[serde(rename = "largeTickHits")]
    pub large_tick_hits: i32,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i32,
    pub grade: V1Grade,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
    #[serde(rename = "hitErrorArray")]
    pub hit_error_array: Arc<[i16]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Grade {
    pub current: String,
    #[serde(rename = "maxThisPlay")]
    pub max_this_play: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1GameplayPp {
    pub current: f32,
    pub fc: f32,
    #[serde(rename = "maxThisPlay")]
    pub max_this_play: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1KeyOverlay {
    pub k1: V1KeyButton,
    pub k2: V1KeyButton,
    pub m1: V1KeyButton,
    pub m2: V1KeyButton,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct V1KeyButton {
    #[serde(rename = "isPressed")]
    pub is_pressed: bool,
    pub count: i32,
}

impl V1KeyButton {
    /// The value every fallback in tosu's builder produces: `?? false` and `?? 0`
    /// on a missing array element. Public because it is the expected reading for
    /// any button the read could not resolve, which makes it a meaningful value to
    /// compare against rather than an internal detail.
    pub const UNPRESSED: Self = Self {
        is_pressed: false,
        count: 0,
    };
}

impl V1KeyOverlay {
    /// A straight field-for-field copy of the one key-overlay read.
    ///
    /// The two shapes are identical, so this is a rename rather than a
    /// conversion -- but they are separate types on purpose: v1 is a wire
    /// contract transcribed from `buildResult.ts` and the v2-side type is
    /// carried on the packet, and letting one borrow the other would tie the v1
    /// wire shape to a field on the v2 packet.
    fn from(overlay: &crate::v2::KeyOverlay) -> Self {
        let button = |b: &crate::v2::KeyOverlayButton| V1KeyButton {
            is_pressed: b.is_pressed,
            count: b.count,
        };
        Self {
            k1: button(&overlay.k1),
            k2: button(&overlay.k2),
            m1: button(&overlay.m1),
            m2: button(&overlay.m2),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Leaderboard {
    #[serde(rename = "hasLeaderboard")]
    pub has_leaderboard: bool,
    #[serde(rename = "isVisible")]
    pub is_visible: bool,
    pub ourplayer: V1LeaderboardPlayer,
    pub slots: Vec<V1LeaderboardPlayer>,
}

/// The 14 keys in `buildResult.ts:22-35` order, which is also the wire order.
///
/// The derived `Default` is what an absent scoreboard looks like on the wire, and it
/// is not all-zero by accident: tosu hardcodes `geki` and `katu` to `0` for stable
/// before converting (`memory/stable.ts:1294-1295`), so a real populated
/// leaderboard still reports `0` for both on stable. Reproducing the zero is
/// correct; reporting rtosu's own counts would not match tosu.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct V1LeaderboardPlayer {
    pub name: String,
    pub score: i32,
    pub combo: i32,
    #[serde(rename = "maxCombo")]
    pub max_combo: i32,
    pub mods: String,
    #[serde(rename = "h300")]
    pub h300: i32,
    pub geki: i32,
    #[serde(rename = "h100")]
    pub h100: i32,
    pub katu: i32,
    #[serde(rename = "h50")]
    pub h50: i32,
    #[serde(rename = "h0")]
    pub h0: i32,
    /// Numeric, not a team name: `convertMemoryPlayerToResult` passes
    /// `memoryPlayer.team` straight through (`buildResult.ts:33`) and a live
    /// response has the integer.
    pub team: i32,
    pub position: i32,
    #[serde(rename = "isPassing")]
    pub is_passing: i32,
}

/// v1's `resultsScreen` is a different shape from v2's: the six legacy judgements
/// sit at the top level rather than under `hits`, `maxCombo` and `mods` are split
/// out, the grade key is `grade` where v2 says `rank`, and there is no `scoreId` or
/// `pp` at all. `mode` comes from **gameplay**, not from the result screen
/// (`buildResult.ts:222`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1ResultsScreen {
    #[serde(rename = "0")]
    pub n0: i32,
    #[serde(rename = "50")]
    pub n50: i32,
    #[serde(rename = "100")]
    pub n100: i32,
    #[serde(rename = "300")]
    pub n300: i32,
    pub mode: i32,
    pub name: String,
    pub score: i32,
    pub accuracy: f64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i32,
    pub mods: V1Mods,
    pub geki: i32,
    pub katu: i32,
    pub grade: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1UserProfile {
    #[serde(rename = "rawLoginStatus")]
    pub raw_login_status: i32,
    pub name: String,
    pub accuracy: f64,
    #[serde(rename = "rankedScore")]
    pub ranked_score: i64,
    pub id: i32,
    pub level: f64,
    #[serde(rename = "playCount")]
    pub play_count: i32,
    #[serde(rename = "playMode")]
    pub play_mode: i32,
    pub rank: i32,
    /// The **numeric** country code, not the name. v2 sends a
    /// {number, name} pair, which makes it easy to assume v1 wants the name; a
    /// live v1 response has the integer.
    #[serde(rename = "countryCode")]
    pub country_code: i32,
    #[serde(rename = "performancePoints")]
    pub performance_points: i32,
    #[serde(rename = "rawBanchoStatus")]
    pub raw_bancho_status: i32,
    #[serde(rename = "backgroundColour")]
    pub background_colour: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Tourney {
    pub manager: V1TourneyManager,
    #[serde(rename = "ipcClients")]
    pub ipc_clients: Vec<V1TourneyIpcClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1TourneyManager {
    #[serde(rename = "ipcState")]
    pub ipc_state: i32,
    #[serde(rename = "bestOF")]
    pub best_of: i32,
    #[serde(rename = "teamName")]
    pub team_name: V1LeftRight<String>,
    pub stars: V1LeftRight<i32>,
    pub bools: V1TourneyBools,
    pub chat: Vec<V1TourneyChatMessage>,
    pub gameplay: V1TourneyGameplay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1LeftRight<T> {
    pub left: T,
    pub right: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1TourneyBools {
    #[serde(rename = "scoreVisible")]
    pub score_visible: bool,
    #[serde(rename = "starsVisible")]
    pub stars_visible: bool,
}

/// v1's chat message is **not** v2's: the text key is `messageBody` rather than
/// `message`, and the time key is `time` rather than `timestamp`. The order is
/// `team, time, name, messageBody` (`buildResult.ts:516-523`), which is also the
/// order the v2 chat uses for its first key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1TourneyChatMessage {
    pub team: String,
    pub time: String,
    pub name: String,
    #[serde(rename = "messageBody")]
    pub message_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1TourneyGameplay {
    pub score: V1LeftRight<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1TourneyIpcClient {
    pub team: String,
    pub spectating: V1Spectating,
    pub gameplay: V1IpcGameplay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1Spectating {
    pub name: String,
    pub country: String,
    #[serde(rename = "userID")]
    pub user_id: i32,
    pub accuracy: f32,
    #[serde(rename = "rankedScore")]
    pub ranked_score: i64,
    #[serde(rename = "playCount")]
    pub play_count: i32,
    #[serde(rename = "globalRank")]
    pub global_rank: i32,
    #[serde(rename = "totalPP")]
    pub total_pp: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1IpcGameplay {
    #[serde(rename = "gameMode")]
    pub game_mode: i32,
    pub name: String,
    pub score: i32,
    pub accuracy: f64,
    pub combo: crate::v2::ComboState,
    pub hp: crate::v2::HealthBarState,
    pub hits: V1IpcHits,
    pub mods: V1Mods,
}

/// Same thirteen keys as [`V1GameplayHits`], and the same reason for it: the three
/// tick counters are on the wire (`buildResult.ts:484-497`) and missing from the
/// declared interface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V1IpcHits {
    #[serde(rename = "0")]
    pub n0: i32,
    #[serde(rename = "50")]
    pub n50: i32,
    #[serde(rename = "100")]
    pub n100: i32,
    #[serde(rename = "300")]
    pub n300: i32,
    pub geki: i32,
    pub katu: i32,
    #[serde(rename = "sliderEndHits")]
    pub slider_end_hits: i32,
    #[serde(rename = "smallTickHits")]
    pub small_tick_hits: i32,
    #[serde(rename = "largeTickHits")]
    pub large_tick_hits: i32,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i32,
    pub grade: V1Grade,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
    #[serde(rename = "hitErrorArray")]
    pub hit_error_array: Arc<[i16]>,
}

/// Build the per-ipc-client hit block from that client's `PlayState`.
///
/// tosu fills all four non-counter fields from the client's own gameplay
/// (`buildResult.ts:484-497`): `grade: {current: gameplay.gradeCurrent,
/// maxThisPlay: gameplay.gradeExpected}`, `unstableRate:
/// gameplay.unstableRate` and `hitErrorArray: gameplay.hitErrors`. Every one of
/// those is already on `TourneyIpcClient.play` -- `rank.current`,
/// `rank.max_this_play`, `unstable_rate` and `hit_error_array` -- one level up.
///
/// This used to convert from `&HitsState`, which structurally cannot carry any
/// of them, so all four were emitted as `""`, `0.0` and `[]` for every client
/// while the same values sat unread in the packet. Converting from the
/// `PlayState` is also what keeps the two paths honest: a field added to
/// `PlayState` now reaches v1 without a second edit here.
impl From<&crate::v2::PlayState> for V1IpcHits {
    fn from(play: &crate::v2::PlayState) -> Self {
        let h = &play.hits;
        Self {
            n0: h.n0,
            n50: h.n50,
            n100: h.n100,
            n300: h.n300,
            geki: h.geki,
            katu: h.katu,
            slider_end_hits: h.slider_end_hits,
            small_tick_hits: h.small_tick_hits,
            large_tick_hits: h.large_tick_hits,
            slider_breaks: h.slider_breaks,
            grade: V1Grade {
                current: play.rank.current.clone(),
                max_this_play: play.rank.max_this_play.clone(),
            },
            unstable_rate: play.unstable_rate,
            hit_error_array: Arc::clone(&play.hit_error_array),
        }
    }
}

impl GosuCompatibleApi {
    /// Reshape a v2 packet into the v1 wire shape.
    ///
    /// Pure, so the whole mapping is testable without osu! running, and cheap:
    /// everything is either a copy of a value already held or a reference to the
    /// graph the session already computed.
    pub fn from_v2(packet: &TosuV2Packet) -> Self {
        let b = &packet.beatmap;
        let stats = &b.stats;
        let play = &packet.play;
        let results = &packet.results_screen;

        // The graph is held pre-serialised (`PrecomputedGraph` wraps a
        // `RawValue`) so v2 does not re-encode it every poll, and **decoded
        // alongside it** so the two reshapers do not re-parse it per request
        // either. `strains_all` is the whole graph object and `strains` is its
        // first series, so one shared decode serves both.
        //
        // The clone here is of the `Arc`, not of the graph: this used to be
        // `graph.series.clone()` and `graph.xaxis.clone()` on an owned local that
        // was never used again, which is two full deep copies of a ~250 KB
        // structure per `/json` request and per `/ws` frame.
        let graph = packet.performance.graph.decoded();
        let strains_all = V1StrainsAll {
            series: graph.series.to_vec(),
            xaxis: graph.xaxis.to_vec(),
        };
        // tosu's `beatmapPP.strains` is the mode's primary skill, zero padded. For
        // osu!std that is the aim series, which is what `series[0]` holds; for any
        // other mode rtosu emits no series at all, so this is empty rather than
        // wrong. See the BLOCKED note on the reading strain in audit-1.0.5.md G-03.
        //
        // Out of the same `Vec` the `strains_all` copy came from, so this is a
        // slice copy rather than a second descent into the graph.
        let strains = strains_all
            .series
            .first()
            .map(|s| s.data.clone())
            .unwrap_or_default();

        Self {
            client: packet.client.clone(),

            settings: V1Settings {
                // tosu reads osu! stable's interface-visibility flag
                // (`states/global.ts:62` <- `memory/stable.ts:814-821`) and rtosu has
                // no equivalent read. False is the neutral value and matches a
                // client with the interface shown. The same read would also close a
                // v2 `game.interfaceVisible` leaf; both are inside the accepted
                // `settings.*` divergence, so nothing is opened for it here.
                show_interface: false,
                folders: V1Folders {
                    game: packet.folders.game.clone(),
                    skin: packet.folders.skin.clone(),
                    songs: packet.folders.songs.clone(),
                },
            },

            menu: V1Menu {
                main_menu: V1MainMenu {
                    // tosu walks 40 audio-velocity floats
                    // (`memory/stable.ts:224-230`) and serves 0 when it cannot.
                    // Verified against a live response, which reported 0.
                    bass_density: 0.0,
                },
                state: packet.state.number,
                // tosu's v1 `menu.gameMode` is `menu.gamemode`
                // (`buildResult.ts:93`), which is the game's **current** ruleset
                // read at `baseAddr - 0x33` (`memory/stable.ts:905`) -- not the
                // beatmap's own ruleset, which is what `beatmap.mode` below
                // carries. Using `b.mode.number` here made the two agree and so
                // stopped reporting a converted map, or any ruleset selection
                // that differed from the file.
                game_mode: b.current_ruleset,
                // `Number(Boolean(global.chatStatus))` (`buildResult.ts:88`).
                // rtosu does not read the chat-visibility flag; 0 is what tosu
                // serves when the field is false.
                is_chat_enabled: 0,
                bm: V1MenuBeatmap {
                    time: V1MenuBeatmapTime {
                        first_obj: b.time.first_object,
                        current: packet.session.play_time,
                        full: b.time.last_object,
                        mp3: b.time.mp3_length,
                    },
                    id: b.id,
                    set: b.set,
                    md5: b.checksum.clone(),
                    ranked_status: b.status.number,
                    metadata: V1Metadata {
                        artist: b.artist.clone(),
                        artist_original: b.artist_unicode.clone(),
                        title: b.title.clone(),
                        title_original: b.title_unicode.clone(),
                        mapper: b.mapper.clone(),
                        difficulty: b.version.clone(),
                    },
                    stats: V1MenuStats {
                        // Converted values, i.e. post-mods, matching tosu's
                        // `arConverted` family rather than the raw file values.
                        ar: stats.ar.converted,
                        cs: stats.cs.converted,
                        od: stats.od.converted,
                        hp: stats.hp.converted,
                        // `currAttributes.stars`, the live partial, not the total.
                        sr: stats.stars.live,
                        bpm: V1Bpm {
                            realtime: stats.bpm.realtime,
                            common: stats.bpm.common,
                            min: stats.bpm.min,
                            max: stats.bpm.max,
                        },
                        circles: stats.objects.circles,
                        sliders: stats.objects.sliders,
                        spinners: stats.objects.spinners,
                        holds: stats.objects.holds,
                        max_combo: stats.max_combo,
                        full_sr: stats.stars.total,
                        // The raw file values, which is what the "memory" prefix
                        // means: `calculatedMapAttributes.ar` is the .osu value.
                        memory_ar: stats.ar.original,
                        memory_cs: stats.cs.original,
                        memory_od: stats.od.original,
                        memory_hp: stats.hp.original,
                    },
                    path: V1MenuPath {
                        // folder + background filename. Not the `.osu` file,
                        // despite `full` suggesting otherwise.
                        //
                        // Through the shared `join_path`, which every other
                        // consumer of this same pair uses. The inline version here
                        // answered `""` only when **both** sides were empty, and
                        // otherwise emitted a leading `\` for an empty folder and a
                        // trailing `\` for an empty filename -- so the same map could
                        // serialise two different strings here and in
                        // `sc.backgroundImageLocation`.
                        full: crate::reader::join_path(&b.folder, &b.background_filename),
                        folder: b.folder.clone(),
                        file: b.filename.clone(),
                        bg: b.background_filename.clone(),
                        audio: b.audio_filename.clone(),
                    },
                },
                mods: V1Mods {
                    num: play.mods.number,
                    str: play.mods.name.clone(),
                },
                pp: V1MenuPp {
                    n90: packet.performance.accuracy.n90,
                    n91: packet.performance.accuracy.n91,
                    n92: packet.performance.accuracy.n92,
                    n93: packet.performance.accuracy.n93,
                    n94: packet.performance.accuracy.n94,
                    n95: packet.performance.accuracy.n95,
                    n96: packet.performance.accuracy.n96,
                    n97: packet.performance.accuracy.n97,
                    n98: packet.performance.accuracy.n98,
                    n99: packet.performance.accuracy.n99,
                    n100: packet.performance.accuracy.n100,
                    strains,
                    strains_all,
                },
            },

            gameplay: V1Gameplay {
                game_mode: play.mode.number,
                name: play.player_name.clone(),
                score: play.score,
                accuracy: play.accuracy,
                combo: play.combo.clone(),
                hp: play.health_bar.clone(),
                hits: V1GameplayHits {
                    n0: play.hits.n0,
                    n50: play.hits.n50,
                    n100: play.hits.n100,
                    n300: play.hits.n300,
                    geki: play.hits.geki,
                    katu: play.hits.katu,
                    slider_end_hits: play.hits.slider_end_hits,
                    small_tick_hits: play.hits.small_tick_hits,
                    large_tick_hits: play.hits.large_tick_hits,
                    slider_breaks: play.hits.slider_breaks,
                    grade: V1Grade {
                        current: play.rank.current.clone(),
                        max_this_play: play.rank.max_this_play.clone(),
                    },
                    unstable_rate: play.unstable_rate,
                    hit_error_array: play.hit_error_array.clone(),
                },
                pp: V1GameplayPp {
                    current: play.pp.current,
                    fc: play.pp.fc,
                    // tosu serves `currAttributes.maxAchievable`, the running maximum
                    // of achieved pp. rtosu's own value for that field is the FC pp,
                    // which is a different quantity; see the live finding in
                    // audit-1.0.5.md I-07. Use the achieved-so-far value here, which
                    // is at least the right shape for a v1 consumer reading a
                    // session best.
                    max_this_play: play.pp.max_achieved,
                },
                // tosu's v1 keyOverlay indexes the read array with `.at(0..3)`
                // and `?? false` / `?? 0` fallbacks, so it always emits four
                // buttons even for taiko's three (`buildResult.ts:194-207`).
                // The values come from the one read in `client::read_key_overlay`;
                // `V1KeyOverlay::from` reproduces the four-key shape, and
                // `audit-1.0.5.md` `B-02` is now closed rather than deferred.
                key_overlay: V1KeyOverlay::from(&play.key_overlay),
                leaderboard: V1Leaderboard {
                    has_leaderboard: false,
                    is_visible: false,
                    ourplayer: V1LeaderboardPlayer::default(),
                    slots: Vec::new(),
                },
                // tosu declares this, initialises it false and never assigns it:
                // the real read lands on `global.isReplayUiHidden`, and v1 reads the
                // gameplay field (`buildResult.ts:219`). A live response confirms
                // false. Emitting the constant matches tosu; implementing the read
                // would not.
                is_replay_ui_hidden: false,
            },

            results_screen: V1ResultsScreen {
                n0: results.hits.n0,
                n50: results.hits.n50,
                n100: results.hits.n100,
                n300: results.hits.n300,
                // `gameplay.mode`, not the result screen's own mode.
                mode: play.mode.number,
                name: results.player_name.clone(),
                score: results.score,
                accuracy: results.accuracy,
                max_combo: results.max_combo,
                mods: V1Mods {
                    num: results.mods.number,
                    str: results.mods.name.clone(),
                },
                geki: results.hits.geki,
                katu: results.hits.katu,
                grade: results.rank.clone(),
                created_at: results.created_at.clone(),
            },

            user_profile: V1UserProfile {
                raw_login_status: packet.profile.user_status.number,
                name: packet.profile.name.clone(),
                accuracy: packet.profile.accuracy,
                ranked_score: packet.profile.ranked_score,
                id: packet.profile.id,
                level: packet.profile.level,
                play_count: packet.profile.play_count,
                play_mode: packet.profile.mode.number,
                rank: packet.profile.global_rank,
                // v1 reports the code as an uppercase name, not the {number, name}
                // pair v2 uses.
                country_code: packet.profile.country_code.number,
                performance_points: packet.profile.pp,
                raw_bancho_status: packet.profile.bancho_status.number,
                background_colour: packet.profile.background_colour.clone(),
            },

            tourney: V1Tourney {
                manager: V1TourneyManager {
                    ipc_state: packet.tourney.ipc_state,
                    best_of: packet.tourney.best_of,
                    team_name: V1LeftRight {
                        left: packet.tourney.team.left.clone(),
                        right: packet.tourney.team.right.clone(),
                    },
                    stars: V1LeftRight {
                        left: packet.tourney.points.left,
                        right: packet.tourney.points.right,
                    },
                    bools: V1TourneyBools {
                        score_visible: packet.tourney.score_visible,
                        stars_visible: packet.tourney.stars_visible,
                    },
                    chat: packet
                        .tourney
                        .chat
                        .iter()
                        .map(|m| V1TourneyChatMessage {
                            team: m.team.clone(),
                            time: m.time.clone(),
                            name: m.name.clone(),
                            message_body: m.message.clone(),
                        })
                        .collect(),
                    gameplay: V1TourneyGameplay {
                        score: V1LeftRight {
                            left: packet.tourney.total_score.left,
                            right: packet.tourney.total_score.right,
                        },
                    },
                },
                ipc_clients: packet
                    .tourney
                    .clients
                    .iter()
                    .map(|c| V1TourneyIpcClient {
                        team: c.team.clone(),
                        spectating: V1Spectating {
                            name: c.user.name.clone(),
                            country: c.user.country.clone(),
                            user_id: c.user.id,
                            accuracy: c.user.accuracy,
                            ranked_score: c.user.ranked_score,
                            play_count: c.user.play_count,
                            global_rank: c.user.global_rank,
                            total_pp: c.user.total_pp,
                        },
                        gameplay: V1IpcGameplay {
                            game_mode: c.play.mode.number,
                            name: c.play.player_name.clone(),
                            score: c.play.score,
                            accuracy: c.play.accuracy,
                            combo: c.play.combo.clone(),
                            hp: c.play.health_bar.clone(),
                            hits: V1IpcHits::from(&c.play),
                            mods: V1Mods {
                                num: c.play.mods.number,
                                str: c.play.mods.name.clone(),
                            },
                        },
                    })
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orders tosu actually emits, captured from a live `/json` response.
    ///
    /// `menu.pp` is the interesting one: `buildResult.ts:157-161` writes the
    /// accuracy spread *after* nothing, and every key in it is an integer-like
    /// string, so `JSON.stringify` sorts all eleven ahead of `strains` and
    /// `strainsAll`. Declaring them in schema order would produce
    /// `strains, strainsAll, 90, 91, …`.
    const EXPECTED_MENU_PP_KEYS: &[&str] = &[
        "90",
        "91",
        "92",
        "93",
        "94",
        "95",
        "96",
        "97",
        "98",
        "99",
        "100",
        "strains",
        "strainsAll",
    ];

    /// Thirteen keys, and the three tick counters are not in the declared
    /// interface at all (`v1.ts:171-182` lists six) but are on the wire.
    const EXPECTED_HITS_KEYS: &[&str] = &[
        "0",
        "50",
        "100",
        "300",
        "geki",
        "katu",
        "sliderEndHits",
        "smallTickHits",
        "largeTickHits",
        "sliderBreaks",
        "grade",
        "unstableRate",
        "hitErrorArray",
    ];

    /// Integer-like keys first, then the string keys in the order the builder
    /// writes them. `mode` comes from gameplay, and the grade key is `grade`.
    const EXPECTED_RESULTS_KEYS: &[&str] = &[
        "0",
        "50",
        "100",
        "300",
        "mode",
        "name",
        "score",
        "accuracy",
        "maxCombo",
        "mods",
        "geki",
        "katu",
        "grade",
        "createdAt",
    ];

    /// Read the key order out of a serialised JSON object without a parser that
    /// would reorder it. Shared with `server`'s route tests, which assert the
    /// same property across payloads.
    fn key_order(json: &str) -> Vec<String> {
        crate::testutil::json_key_order(json)
    }

    #[test]
    fn menu_pp_emits_the_accuracies_ascending_before_the_strain_blocks() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        let order = key_order(&serde_json::to_string(&v1.menu.pp).unwrap());
        assert_eq!(order, EXPECTED_MENU_PP_KEYS, "menu.pp wire order");
    }

    #[test]
    fn gameplay_hits_carries_the_three_undeclared_tick_counters() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        let order = key_order(&serde_json::to_string(&v1.gameplay.hits).unwrap());
        assert_eq!(order, EXPECTED_HITS_KEYS, "gameplay.hits wire order");
    }

    #[test]
    fn results_screen_hoists_the_judgement_counts_and_keeps_grade() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        let order = key_order(&serde_json::to_string(&v1.results_screen).unwrap());
        assert_eq!(order, EXPECTED_RESULTS_KEYS, "resultsScreen wire order");
    }

    #[test]
    fn the_top_level_shape_is_the_seven_keys_tosu_serves() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        let order = key_order(&serde_json::to_string(&v1).unwrap());
        assert_eq!(
            order,
            vec![
                "client",
                "settings",
                "menu",
                "gameplay",
                "resultsScreen",
                "userProfile",
                "tourney"
            ]
        );
    }

    /// `countryCode` and the leaderboard's `team` are integers in v1. Both were
    /// written as strings first and caught only by diffing a live rtosu response
    /// against a live tosu one, because the v2 payload presents `countryCode` as a
    /// `{number, name}` pair and invites the wrong reading.
    #[test]
    fn country_code_and_leaderboard_team_are_numbers() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        let json = serde_json::to_string(&v1).unwrap();
        assert!(
            json.contains("\"countryCode\":0"),
            "countryCode must be the numeric code, found: {json}"
        );
        assert!(
            json.contains("\"team\":0"),
            "ourplayer.team must be numeric, found: {json}"
        );
    }

    /// tosu declares `isReplayUiHidden`, initialises it false and never assigns
    /// it -- the real read lands on `global.isReplayUiHidden` and v1 reads the
    /// gameplay field. A live response confirms false, so the constant is what
    /// matches; implementing a read here would not.
    #[test]
    fn replay_ui_hidden_is_the_constant_tosu_serves() {
        let v1 = GosuCompatibleApi::from_v2(&TosuV2Packet::default());
        assert!(!v1.gameplay.is_replay_ui_hidden);
    }

    /// `menu.gameMode` is the **game's current** ruleset, not the beatmap's own.
    ///
    /// tosu's v1 `menu.gameMode` is `menu.gamemode` (`buildResult.ts:93`), read
    /// at `baseAddr - 0x33` (`memory/stable.ts:905`) -- the value that
    /// `beatmap.mode.number` held before the file's `Mode:` was applied to it.
    /// Taking it from the beatmap made the two agree, so a converted map, or any
    /// ruleset selection differing from the file, reported the wrong gamemode on
    /// `/json` and `/ws`.
    #[test]
    fn menu_game_mode_is_the_current_ruleset_not_the_beatmaps_own() {
        // The two fields hold different values on purpose, so a builder that read
        // the beatmap again fails here.
        let mut packet = TosuV2Packet::default();
        // A converted osu!standard map being played in mania.
        packet.beatmap.current_ruleset = 3;
        packet.beatmap.mode = crate::beatmap::BeatmapMode {
            number: 0,
            name: "osu".to_string(),
        };
        packet.beatmap.is_convert = true;

        let v1 = GosuCompatibleApi::from_v2(&packet);

        assert_eq!(v1.menu.game_mode, 3, "the game's current ruleset");
        assert_eq!(packet.beatmap.mode.number, 0, "the map's own ruleset");
        assert!(packet.beatmap.is_convert);
    }

    /// The per-ipc-client hit block carries that client's grade, unstable rate
    /// and hit errors.
    ///
    /// tosu fills all four per client from that client's gameplay
    /// (`buildResult.ts:484-497`). rtosu converted from `&HitsState`, which
    /// structurally cannot carry any of them, so every client reported `""` /
    /// `0.0` / `[]` while the same values sat unread in `TourneyIpcClient.play`
    /// one level up.
    #[test]
    fn the_ipc_hit_block_carries_each_clients_grade_ur_and_hit_errors() {
        let mut packet = TosuV2Packet::default();
        let mut client = crate::v2::TourneyIpcClient {
            ipc_id: 1,
            ..Default::default()
        };
        client.play.rank.current = "XH".to_string();
        client.play.rank.max_this_play = "S".to_string();
        client.play.unstable_rate = 3.25;
        client.play.hit_error_array = Arc::from(vec![-4i16, 0, 9]);
        client.play.hits.n300 = 412;
        packet.tourney.clients = vec![client];

        let v1 = GosuCompatibleApi::from_v2(&packet);
        let hits = &v1.tourney.ipc_clients[0].gameplay.hits;

        assert_eq!(hits.grade.current, "XH");
        assert_eq!(hits.grade.max_this_play, "S");
        assert_eq!(hits.unstable_rate, 3.25);
        assert_eq!(hits.hit_error_array.as_ref(), &[-4i16, 0, 9]);
        assert_eq!(hits.n300, 412, "the counters still come across");
    }

    /// The key overlay is **four buttons, always**, and the values come from the
    /// one read on the packet.
    ///
    /// tosu indexes the read array positionally with `.at(0..3)` and `?? false` /
    /// `?? 0` fallbacks (`buildResult.ts:194-207`), so a three-element taiko or
    /// catch read still emits four keys. Emitting three would be a shape change
    /// for every consumer that reads the fourth by name.
    ///
    /// The values used to be the neutral default on every button, because rtosu
    /// had no key-state read; that was a recorded gap, not a passing value. The
    /// read exists now (`client::read_key_overlay`), so the count is carried
    /// through rather than dropped on the floor.
    #[test]
    fn the_key_overlay_carries_four_buttons_and_the_reads_values() {
        let json_of = |packet: &TosuV2Packet| {
            let v1 = GosuCompatibleApi::from_v2(packet);
            serde_json::to_string(&v1.gameplay.key_overlay).unwrap()
        };

        // A default packet: the neutral overlay, four keys.
        let empty = json_of(&TosuV2Packet::default());
        assert_eq!(key_order(&empty), vec!["k1", "k2", "m1", "m2"]);
        assert!(empty.contains("\"isPressed\":false"));
        assert!(empty.contains("\"count\":0"));

        // A packet carrying a read: the values survive the reshape.
        let mut packet = TosuV2Packet::default();
        packet.play.key_overlay = crate::v2::KeyOverlay {
            k1: crate::v2::KeyOverlayButton {
                is_pressed: true,
                count: 11,
            },
            k2: crate::v2::KeyOverlayButton {
                is_pressed: false,
                count: 9,
            },
            m1: crate::v2::KeyOverlayButton::default(),
            // taiko has three bindings; `m2` is still emitted, defaulted.
            m2: crate::v2::KeyOverlayButton::default(),
        };
        let v1 = GosuCompatibleApi::from_v2(&packet);
        assert_eq!(v1.gameplay.key_overlay.k1.count, 11);
        assert!(v1.gameplay.key_overlay.k1.is_pressed);
        assert_eq!(v1.gameplay.key_overlay.k2.count, 9);
        assert!(!v1.gameplay.key_overlay.k2.is_pressed);
        assert_eq!(v1.gameplay.key_overlay.m2, V1KeyButton::UNPRESSED);
        assert_eq!(
            key_order(&json_of(&packet)),
            vec!["k1", "k2", "m1", "m2"],
            "still four, whatever the mode"
        );
        assert!(json_of(&packet).contains("\"isPressed\":true"));
    }
}
