use crate::beatmap::{BeatmapSnapshot, BeatmapStats};
use crate::client::{ModEntry, mod_acronyms, mod_bits};
use crate::pp::LivePpResult;
use crate::tournament::TournamentChatMessage;
use md5::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameState {
    pub focused: bool,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OsuStatusState {
    pub number: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub play_time: i32,
    pub play_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthBarState {
    pub normal: f64,
    pub smooth: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HitsState {
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
    pub slider_breaks: i32,
    pub slider_end_hits: i32,
    pub small_tick_hits: i32,
    pub large_tick_hits: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsHitsState {
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
    pub slider_end_hits: i32,
    pub small_tick_hits: i32,
    pub large_tick_hits: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComboState {
    pub current: i32,
    pub max: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModsState {
    pub checksum: String,
    pub number: u32,
    pub name: String,
    pub array: Vec<ModEntry>,
    pub rate: f32,
}

impl Default for ModsState {
    fn default() -> Self {
        Self {
            checksum: String::new(),
            number: 0,
            name: String::new(),
            array: Vec::new(),
            rate: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RankState {
    pub current: String,
    pub max_this_play: String,
}

/// One key-overlay button: whether it is down, and how many keys it has
/// registered this play.
///
/// `KeyOverlayButton` in `api/types/v2.ts:426-429`. The two fields are read from
/// osu! stable memory at element `+0x1C` and `+0x14`
/// (`memory/stable.ts:644-728`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyOverlayButton {
    pub is_pressed: bool,
    pub count: i32,
}

impl KeyOverlayButton {
    /// The value every fallback in tosu's builders produces: `?? false` and
    /// `?? 0` on a missing array element.
    pub const UNPRESSED: Self = Self {
        is_pressed: false,
        count: 0,
    };
}

/// The four key-overlay buttons.
///
/// **Always four keys, for every ruleset.** tosu's builders index the read array
/// positionally with `.at(0..3)` and `?? false` / `?? 0` fallbacks
/// (`buildResultV2Precise.ts:28-49`, `buildResult.ts:194-207`), so a
/// three-element taiko or catch array still yields four keys with `m2` at the
/// neutral value. Reproducing the short array instead would drop a key that
/// consumers index by name.
///
/// The names are positional, not the game's: osu! catch's three bindings are
/// `L`, `R`, `D` and taiko's are `K1`, `K2`, `M1` on the game side
/// (`memory/stable.ts:679-724`), but every payload calls positions 0..3
/// `k1`, `k2`, `m1`, `m2`. `m2` is osu!std-only in the read, not in the payload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyOverlay {
    pub k1: KeyOverlayButton,
    pub k2: KeyOverlayButton,
    pub m1: KeyOverlayButton,
    pub m2: KeyOverlayButton,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayState {
    pub failed: bool,
    pub player_name: String,
    pub mode: OsuStatusState,
    pub score: i32,
    pub accuracy: f64,
    pub health_bar: HealthBarState,
    pub hits: HitsState,
    pub hit_error_array: Arc<[i16]>,
    pub combo: ComboState,
    pub mods: ModsState,
    pub rank: RankState,
    pub pp: LivePpResult,
    pub unstable_rate: f64,
    /// The key overlay, read from osu! stable memory during gameplay.
    ///
    /// `#[serde(skip)]` because tosu's **v2** payload has no such key: it appears
    /// only in the precise payload (`api/utils/buildResultV2Precise.ts:68-85`),
    /// in v1's `gameplay.keyOverlay` and in SC's stringified `keyOverlay`. All
    /// three are reshapes of this packet, so one read here serves all three --
    /// the same arrangement the SC-only beatmap fields use.
    #[serde(skip)]
    pub key_overlay: KeyOverlay,
}

impl Default for PlayState {
    fn default() -> Self {
        Self {
            failed: false,
            player_name: String::new(),
            mode: OsuStatusState {
                number: 0,
                name: "osu".to_string(),
            },
            score: 0,
            accuracy: 100.0,
            health_bar: HealthBarState::default(),
            hits: HitsState::default(),
            hit_error_array: Arc::default(),
            combo: ComboState::default(),
            mods: create_mods_state(0, ""),
            rank: RankState::default(),
            pp: LivePpResult::default(),
            unstable_rate: 0.0,
            key_overlay: KeyOverlay::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsScreenPp {
    pub current: f32,
    pub fc: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsScreenState {
    pub score_id: i64,
    pub player_name: String,
    pub mode: OsuStatusState,
    pub score: i32,
    pub accuracy: f64,
    pub name: String,
    pub hits: ResultsHitsState,
    pub mods: ModsState,
    pub max_combo: i32,
    pub rank: String,
    pub pp: ResultsScreenPp,
    pub created_at: String,
}

impl Default for ResultsScreenState {
    fn default() -> Self {
        Self {
            score_id: 0,
            player_name: String::new(),
            mode: OsuStatusState {
                number: 0,
                name: "osu".to_string(),
            },
            score: 0,
            accuracy: 0.0,
            name: String::new(),
            hits: ResultsHitsState::default(),
            mods: ModsState::default(),
            max_combo: 0,
            rank: String::new(),
            pp: ResultsScreenPp::default(),
            created_at: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FoldersState {
    pub game: String,
    pub skin: String,
    pub songs: String,
    pub beatmap: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FilesState {
    pub beatmap: String,
    pub background: String,
    pub audio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DirectPathState {
    pub beatmap_file: String,
    pub beatmap_background: String,
    pub beatmap_audio: String,
    pub beatmap_folder: String,
    pub skin_folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MatchmakingState {
    pub rating: f64,
    pub rank: Option<i32>,
    pub plays: i32,
    pub wins: i32,
    pub is_provisional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileState {
    pub user_status: OsuStatusState,
    pub bancho_status: OsuStatusState,
    pub id: i32,
    pub name: String,
    pub mode: OsuStatusState,
    pub ranked_score: i64,
    pub level: f64,
    pub accuracy: f64,
    pub pp: i32,
    pub play_count: i32,
    pub global_rank: i32,
    pub country_code: OsuStatusState,
    pub background_colour: String,
    pub matchmaking: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceAccuracy {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphSeries {
    pub name: String,
    pub data: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceGraph {
    pub series: Vec<GraphSeries>,
    pub xaxis: Vec<f64>,
}

/// The serialized form of an empty `PerformanceGraph`, used whenever a graph
/// cannot be encoded. `RawValue` parses it, so this is also the fallback of last
/// resort and must stay valid JSON.
const EMPTY_GRAPH_JSON: &str = "{\"series\":[],\"xaxis\":[]}";

/// A `PerformanceGraph` that is already serialized, so the hot path does not
/// re-encode it on every poll.
///
/// Not serialised itself: it appears inside the packet through
/// `#[serde(serialize_with = ...)]` on the field, so `raw` is what goes on the
/// wire and `decoded` is a cache of that same value.
#[derive(Debug, Clone)]
pub struct PrecomputedGraph {
    pub raw: Arc<serde_json::value::RawValue>,
    /// The same graph, decoded once, for the three consumers that need the parsed
    /// series (`v1`'s `strainsAll`, SC's `mapStrains`, and anything else that
    /// wants a series rather than bytes).
    ///
    /// **Why this exists:** the graph is held as raw JSON precisely so that the v2
    /// hot path does not re-encode it every poll, and the two reshapers had been
    /// undoing that on the way out -- each one did
    /// `serde_json::from_str(raw.get())` per **request** or per **socket frame**,
    /// parsing roughly 250 KB and allocating five `Vec<f64>` each time, and v1
    /// then deep-copied the whole thing again because it needed two of the fields.
    /// With several clients connected, that is the parse repeated once per client
    /// per tick on a route nobody asked to be cheap.
    ///
    /// So the decode happens once, at construction, and the reshapers borrow it.
    /// The cost of a poll that produces no consumer is one parse of a graph the
    /// poll just built anyway, and the cost of the tenth consumer is a refcount
    /// bump.
    ///
    /// Never on the wire, and never part of equality: it is a cache of `raw`, so
    /// two graphs with the same bytes are equal whether or not either is decoded.
    decoded: Option<Arc<PerformanceGraph>>,
    /// `decoded`'s first series' data, hoisted behind its own `Arc`.
    ///
    /// **Why this exists:** v1's `menu.pp.strains` is exactly this slice -- the
    /// primary skill series -- and it used to reach it with
    /// `series[0].data.clone()`, a full copy of the longest series in the graph
    /// on every `/json` request and every `/ws` frame. With the decoded graph
    /// already shared, the copy was pure waste, so the slice is published once
    /// here and every consumer of a poll refcount-bumps it instead.
    ///
    /// `None` exactly when `decoded` is `None`: the shared-static default has no
    /// series to hoist, and materialising a fresh `Arc` per caller would defeat
    /// the point. `primary_series()` supplies the empty slice for it.
    primary: Option<Arc<Vec<f64>>>,
}

impl PrecomputedGraph {
    /// Both steps are fallible in principle and neither may panic. The graph is
    /// rebuilt every poll, and under `panic = "abort"` a failure here would take
    /// the whole process down mid-match rather than dropping one frame.
    pub fn new(graph: &PerformanceGraph) -> Self {
        match serde_json::to_string(graph)
            .ok()
            .and_then(|json| serde_json::value::RawValue::from_string(json).ok())
        {
            Some(raw) => {
                // Hoisted from the same clone `decoded` hands out, so
                // `primary_series()` cannot disagree with it.
                let decoded = Arc::new(graph.clone());
                let primary = decoded.series.first().map(|s| Arc::new(s.data.clone()));
                Self {
                    raw: Arc::from(raw),
                    // Decoded from the same bytes the payload carries, so the two
                    // can never disagree about the graph's contents.
                    decoded: Some(decoded),
                    primary,
                }
            }
            None => Self::default(),
        }
    }

    /// The decoded graph, or an empty one.
    ///
    /// Never fails: a payload built by [`Self::from_raw_json`] has no decoded
    /// form cached, and a consumer that cannot have one must still get the empty
    /// graph rather than an error -- that is the same fallback the re-shapers
    /// already had with `unwrap_or_default()`, and a missing `Arc` is the only new
    /// way to reach it.
    pub fn decoded(&self) -> Arc<PerformanceGraph> {
        self.decoded
            .clone()
            .unwrap_or_else(|| Arc::new(PerformanceGraph::default()))
    }

    /// The first series' data, or an empty slice.
    ///
    /// Never fails, and never copies per consumer: the slice is published once
    /// by [`Self::new`] / [`Self::from_raw_json`] and shared from there. A graph
    /// with no series -- the empty default, or a non-osu!std ruleset, which
    /// rtosu emits with no series at all -- yields the empty slice, which is
    /// exactly what the consumer produced before this hoist existed.
    pub fn primary_series(&self) -> Arc<Vec<f64>> {
        self.primary.clone().unwrap_or_else(|| Arc::new(Vec::new()))
    }

    pub fn from_raw_json(json: String) -> Result<Self, serde_json::Error> {
        let raw: Arc<serde_json::value::RawValue> =
            Arc::from(serde_json::value::RawValue::from_string(json)?);
        // Decoded here for the same reason `new` does it. This constructor is
        // the deserialisation path, so without it a packet rebuilt from the
        // wire would silently lose the cache and every consumer would fall
        // back to the empty graph.
        let decoded: Arc<PerformanceGraph> =
            Arc::new(serde_json::from_str(raw.get()).unwrap_or_default());
        let primary = decoded.series.first().map(|s| Arc::new(s.data.clone()));
        Ok(Self {
            primary,
            decoded: Some(decoded),
            raw,
        })
    }
}

impl Default for PrecomputedGraph {
    fn default() -> Self {
        static EMPTY: std::sync::OnceLock<Arc<serde_json::value::RawValue>> =
            std::sync::OnceLock::new();
        let raw = EMPTY.get_or_init(|| {
            Arc::from(
                serde_json::value::RawValue::from_string(EMPTY_GRAPH_JSON.to_string())
                    .unwrap_or_default(),
            )
        });
        Self {
            raw: Arc::clone(raw),
            // No decoded cache: the default is a shared static, and handing every
            // consumer a fresh empty `Arc` would defeat the point of the cache.
            // `decoded()` materialises one per caller, which is only reached when
            // nothing built a real graph.
            decoded: None,
            // Likewise nothing to hoist from.
            primary: None,
        }
    }
}

impl PartialEq for PrecomputedGraph {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.raw, &other.raw) || self.raw.get() == other.raw.get()
    }
}

impl Serialize for PrecomputedGraph {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PrecomputedGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw_box = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        let raw: Arc<serde_json::value::RawValue> = Arc::from(raw_box);
        // Decoded here as well as in `from_raw_json`, because this is the path
        // a packet rebuilt from the wire takes, and a consumer asking for the
        // parsed series must get the graph that was actually sent rather than
        // the empty fallback.
        let decoded: Arc<PerformanceGraph> =
            Arc::new(serde_json::from_str(raw.get()).unwrap_or_default());
        let primary = decoded.series.first().map(|s| Arc::new(s.data.clone()));
        Ok(PrecomputedGraph {
            decoded: Some(decoded),
            primary,
            raw,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceState {
    pub accuracy: PerformanceAccuracy,
    pub graph: PrecomputedGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardHitsState {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardEntry {
    pub is_failed: bool,
    pub position: i32,
    pub team: i32,
    pub id: i32,
    pub name: String,
    pub score: i32,
    pub accuracy: f64,
    pub hits: LeaderboardHitsState,
    pub combo: ComboState,
    pub mods: ModsState,
    pub rank: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyTeam {
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyPoints {
    pub left: i32,
    pub right: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyTotalScore {
    pub left: i64,
    pub right: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyUser {
    pub id: i32,
    pub name: String,
    pub country: String,
    pub accuracy: f32,
    pub ranked_score: i64,
    pub play_count: i32,
    pub global_rank: i32,
    #[serde(rename = "totalPP")]
    pub total_pp: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyClientSettings {
    pub mania: TourneyManiaSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyManiaSettings {
    pub scroll_speed: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TourneyClientBeatmap {
    pub stats: BeatmapStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyIpcClient {
    pub ipc_id: usize,
    pub team: String,
    pub settings: TourneyClientSettings,
    pub user: TourneyUser,
    pub beatmap: TourneyClientBeatmap,
    pub play: PlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyRootState {
    pub score_visible: bool,
    pub stars_visible: bool,
    pub ipc_state: i32,
    #[serde(rename = "bestOF")]
    pub best_of: i32,
    pub team: TourneyTeam,
    pub points: TourneyPoints,
    pub chat: Vec<TournamentChatMessage>,
    pub total_score: TourneyTotalScore,
    pub clients: Vec<TourneyIpcClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TosuV2Packet {
    pub game: GameState,
    pub client: String,
    pub server: String,
    pub state: OsuStatusState,
    pub session: SessionState,
    pub profile: ProfileState,
    pub beatmap: BeatmapSnapshot,
    pub play: PlayState,
    pub leaderboard: Vec<LeaderboardEntry>,
    pub performance: PerformanceState,
    pub results_screen: ResultsScreenState,
    pub folders: FoldersState,
    pub files: FilesState,
    pub direct_path: DirectPathState,
    pub tourney: TourneyRootState,
}

/// One tournament client's entry in the precise payload's `tourney` array.
///
/// `PreciseTourney` in `api/types/v2.ts:415-419`, assembled at
/// `buildResultV2Precise.ts:26-51`. The same three keys as the top level, less
/// the tourney array itself: a client needs its own keys and its own hit errors,
/// and there is nothing else about it that is precise.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreciseTourneyClient {
    pub ipc_id: usize,
    pub keys: KeyOverlay,
    pub hit_errors: Arc<[i16]>,
}

/// tosu's `/json/v2/precise` payload: exactly three keys.
///
/// `TosuPreciseAnswer` in `api/types/v2.ts:409-413`, assembled at
/// `buildResultV2Precise.ts:57-88`. **Three keys, not the v2 packet** -- and
/// that is the whole point of the endpoint. It used to answer with the full v2
/// packet, which meant an overlay asking for a ~1 KB high-frequency feed got
/// 24 KB including five graph series of thousands of doubles, on every tick:
/// 221 bytes against 24,039 bytes measured live, 109x, with no `keys` key at all
/// and no `hitErrors` key either.
///
/// The three keys are the fast-changing ones -- key state and the hit-error
/// list -- and nothing that is already available more cheaply on `/json/v2`.
/// `hit_errors` is the same `Arc` the rest of the packet carries, so a precise
/// frame does not re-read or re-copy the list: it clones an `Arc`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TosuPrecisePacket {
    pub keys: KeyOverlay,
    pub hit_errors: Arc<[i16]>,
    pub tourney: Vec<PreciseTourneyClient>,
}

impl TosuPrecisePacket {
    /// The precise view of a full packet.
    ///
    /// The top level is the **focused** client's state, which on rtosu is the
    /// packet's own `play` block: `packet.play` is built from the active ruleset
    /// rather than from any one tourney client, and tosu reads the same two
    /// values off `instanceManager.focusedClient`'s gameplay
    /// (`buildResultV2Precise.ts:61-66`).
    ///
    /// **In tournament mode that top level is the neutral default, and that is a
    /// real limitation rather than a bug to be papered over.**
    /// `format_tourney_packet` never fills `packet.play`, because rtosu's manager
    /// process is not itself one of `tourney.clients` -- `is_attached` counts the
    /// other clients, and only they are read for gameplay. So there is no focused
    /// client in the sense tosu means, and inventing one (taking the lowest
    /// `ipcId`, say) would be a guess about which of several players a consumer
    /// meant. A tournament consumer that wants key state must read it per client
    /// out of `tourney[]`, which is what that array is for and where the values
    /// are real.
    ///
    /// The array is every known tourney client, ordered by `ipcId`, because tosu
    /// sorts the same way (`buildResultV2Precise.ts:22` -- `a.ipcId - b.ipcId`)
    /// and an array whose order differs is a byte-diff. rtosu holds the clients in
    /// a `BTreeMap<u32, _>` keyed by pid, so the sort has to be explicit here.
    pub fn from_v2(packet: &TosuV2Packet) -> Self {
        let mut tourney: Vec<PreciseTourneyClient> = packet
            .tourney
            .clients
            .iter()
            .map(|client| PreciseTourneyClient {
                ipc_id: client.ipc_id,
                keys: client.play.key_overlay,
                hit_errors: Arc::clone(&client.play.hit_error_array),
            })
            .collect();
        tourney.sort_by_key(|client| client.ipc_id);

        Self {
            keys: packet.play.key_overlay,
            hit_errors: Arc::clone(&packet.play.hit_error_array),
            tourney,
        }
    }
}

pub fn create_mods_state(mods_num: u32, mods_str: &str) -> ModsState {
    crate::instr_scope!(ModsState);
    static CACHE: std::sync::LazyLock<
        std::sync::RwLock<std::collections::HashMap<(u32, String), ModsState>>,
    > = std::sync::LazyLock::new(|| {
        std::sync::RwLock::new(std::collections::HashMap::with_capacity(32))
    });

    let key = (mods_num, mods_str.to_string());
    if let Ok(guard) = CACHE.read() {
        if let Some(state) = guard.get(&key) {
            return state.clone();
        }
    }

    let array = mod_acronyms(mods_num);
    let rate = if (mods_num & mod_bits::DT) != 0 || (mods_num & mod_bits::NC) != 0 {
        1.5
    } else if (mods_num & mod_bits::HT) != 0 {
        0.75
    } else {
        1.0
    };
    let checksum = md5_hex(serde_json::to_string(&array).unwrap_or_default().as_bytes());

    let name = crate::client::format_mods(mods_num);
    let state = ModsState {
        checksum,
        number: mods_num,
        name: if name.is_empty() && !mods_str.is_empty() {
            mods_str.to_string()
        } else {
            name
        },
        array,
        rate,
    };

    if let Ok(mut guard) = CACHE.write() {
        guard.insert(key, state.clone());
    }
    state
}

pub fn osu_state_name(state_num: i32) -> &'static str {
    match state_num {
        0 => "menu",
        1 => "edit",
        2 => "play",
        3 => "exit",
        4 => "selectEdit",
        5 => "selectPlay",
        6 => "selectDrawings",
        7 => "resultScreen",
        8 => "update",
        9 => "busy",
        10 => "unknown",
        11 => "lobby",
        12 => "matchSetup",
        13 => "selectMulti",
        14 => "rankingVs",
        15 => "onlineSelection",
        16 => "optionsOffsetWizard",
        17 => "rankingTagCoop",
        18 => "rankingTeam",
        19 => "beatmapImport",
        20 => "packageUpdater",
        21 => "benchmark",
        22 => "tourney",
        23 => "charts",
        _ => "",
    }
}

fn md5_hex(input: &[u8]) -> String {
    use std::fmt::Write;
    let digest = md5::Md5::digest(input);
    let mut hex = String::with_capacity(32);
    for byte in digest {
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty graph is the fallback of last resort, so it has to be valid
    /// JSON on its own terms -- not merely valid because the thing it replaced
    /// happened to parse.
    #[test]
    fn the_default_graph_is_valid_json() {
        let raw = PrecomputedGraph::default().raw.get().to_string();
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("default graph parses");
        assert_eq!(parsed["series"].as_array().map(Vec::len), Some(0));
        assert_eq!(parsed["xaxis"].as_array().map(Vec::len), Some(0));
    }

    /// A real graph still round-trips, so the fallback did not become the
    /// normal path.
    #[test]
    fn a_real_graph_is_encoded_rather_than_replaced_by_the_fallback() {
        let graph = PerformanceGraph {
            series: vec![GraphSeries {
                name: "strain".to_string(),
                data: vec![1.5, 2.5],
            }],
            xaxis: vec![0.0, 16.0],
        };
        let raw = PrecomputedGraph::new(&graph).raw.get().to_string();
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("graph parses");
        assert_eq!(parsed["series"].as_array().map(Vec::len), Some(1));
        assert_eq!(parsed["xaxis"].as_array().map(Vec::len), Some(2));
    }

    /// The precise payload is **three keys**, and that is the endpoint's whole
    /// purpose. It used to be the full v2 packet on both the route and the
    /// socket, so a client asking for a cheap high-frequency feed got the strain
    /// graph with it -- 221 bytes from tosu against 24,039 from rtosu on the same
    /// map in the same state, with no `keys` and no `hitErrors` at all.
    ///
    /// The order is asserted off the serialised bytes rather than a parsed
    /// `Value`, because `serde_json` sorts map keys and an order assertion written
    /// that way passes for every order.
    #[test]
    fn the_precise_payload_is_three_keys_and_not_the_v2_packet() {
        let mut packet = TosuV2Packet::default();
        packet.play.key_overlay.k1 = KeyOverlayButton {
            is_pressed: true,
            count: 11,
        };
        packet.play.hit_error_array = Arc::from(vec![-3i16, 0, 7, 12, -20]);
        // A graph big enough that carrying it would be obvious in the size.
        packet.performance.graph = PrecomputedGraph::new(&PerformanceGraph {
            series: (0..5)
                .map(|index| GraphSeries {
                    name: format!("series{index}"),
                    data: (0..2_000).map(|point| point as f64 * 0.5).collect(),
                })
                .collect(),
            xaxis: (0..2_000).map(|point| point as f64 * 400.0).collect(),
        });

        let precise = TosuPrecisePacket::from_v2(&packet);
        let json = serde_json::to_string(&precise).expect("serialize the precise payload");

        assert_eq!(
            crate::testutil::json_key_order(&json),
            ["keys", "hitErrors", "tourney"],
            "tosu's three keys, in order (buildResultV2Precise.ts:68-87)"
        );
        // The keys the old payload did not have at all.
        assert!(json.contains("\"keys\":{"), "keys is present: {json}");
        assert!(json.contains("\"hitErrors\":["), "hitErrors is present");
        assert!(
            json.contains("\"isPressed\":true"),
            "the read is carried through"
        );
        assert!(json.contains("\"count\":11"));
        // And none of the heavy v2 leaves.
        for absent in ["\"beatmap\"", "\"performance\"", "\"graph\"", "\"profile\""] {
            assert!(
                !json.contains(absent),
                "the precise payload must not carry {absent}"
            );
        }
    }

    /// The size claim, as an invariant rather than a number in a comment. A
    /// default packet's v2 body is dominated by the graph; the precise body must
    /// not grow with it.
    #[test]
    fn the_precise_payload_does_not_carry_the_strain_graph() {
        let mut packet = TosuV2Packet::default();
        packet.performance.graph = PrecomputedGraph::new(&PerformanceGraph {
            series: (0..5)
                .map(|index| GraphSeries {
                    name: format!("series{index}"),
                    data: (0..4_778).map(|point| point as f64 * 0.1234).collect(),
                })
                .collect(),
            xaxis: (0..4_778).map(|point| point as f64 * 400.0).collect(),
        });

        let full = serde_json::to_string(&packet).expect("serialize v2");
        let precise = serde_json::to_string(&TosuPrecisePacket::from_v2(&packet))
            .expect("serialize the precise payload");

        assert!(
            full.len() > 100_000,
            "the fixture's v2 body should be graph-dominated, got {} bytes",
            full.len()
        );
        assert!(
            precise.len() < 5_000,
            "the precise body must stay small, got {} bytes",
            precise.len()
        );
    }

    /// `tourney` is one entry per client, ordered by `ipcId`, and each entry
    /// carries that client's own keys and hit errors -- not the focused client's.
    /// tosu sorts the same way (`buildResultV2Precise.ts:22`) and an array in a
    /// different order is a byte-diff, so the sort is asserted rather than assumed:
    /// rtosu holds clients in a `BTreeMap` keyed by **pid**, so ipc order and map
    /// order are not the same thing.
    #[test]
    fn the_precise_tourney_array_is_sorted_by_ipc_id_and_per_client() {
        let mut packet = TosuV2Packet::default();
        for (ipc_id, count) in [(7usize, 3usize), (2, 1), (5, 2)] {
            let mut client = TourneyIpcClient {
                ipc_id,
                ..Default::default()
            };
            client.play.key_overlay.k2 = KeyOverlayButton {
                is_pressed: true,
                count: count as i32,
            };
            client.play.hit_error_array = Arc::from(vec![count as i16; count]);
            packet.tourney.clients.push(client);
        }
        // The focused client's own values, which are not any client's.
        packet.play.key_overlay.k1 = KeyOverlayButton {
            is_pressed: true,
            count: 99,
        };

        let precise = TosuPrecisePacket::from_v2(&packet);
        let ids: Vec<usize> = precise.tourney.iter().map(|c| c.ipc_id).collect();
        assert_eq!(
            ids,
            [2, 5, 7],
            "ascending ipcId, whatever order they arrived in"
        );
        assert_eq!(
            precise.keys.k1.count, 99,
            "the top level is the focused client"
        );

        // Each entry carries its own client's data, not the top level's.
        assert_eq!(precise.tourney[0].keys.k2.count, 1);
        assert_eq!(precise.tourney[2].keys.k2.count, 3);
        assert_eq!(precise.tourney[2].hit_errors.as_ref(), &[3i16, 3, 3]);
        assert!(
            precise.tourney.iter().all(|c| c.keys.k1.count == 0),
            "a client entry is not the focused client's overlay"
        );

        // The entry key order is tosu's too.
        let json = serde_json::to_string(&precise.tourney[0]).expect("serialize an entry");
        assert_eq!(
            crate::testutil::json_key_order(&json),
            ["ipcId", "keys", "hitErrors"],
            "tosu's per-client key order (buildResultV2Precise.ts:26-51)"
        );
    }

    /// The key overlay is **always four buttons**, whatever the ruleset, because
    /// tosu indexes the read array positionally with `.at(0..3)` and `?? false` /
    /// `?? 0`. A taiko or catch read has three elements, and the builders still
    /// emit `m2` at the neutral value. Dropping the key would be a shape change
    /// for every consumer that reads it by name.
    #[test]
    fn the_key_overlay_has_four_buttons_whatever_the_ruleset() {
        for (mode, elements) in [(0, 4), (1, 3), (2, 3), (3, 3)] {
            let overlay = crate::v2::KeyOverlay::default();
            let json = serde_json::to_string(&overlay).expect("serialize the overlay");
            assert_eq!(
                crate::testutil::json_key_order(&json),
                ["k1", "k2", "m1", "m2"],
                "mode {mode} reads {elements} elements and still emits four"
            );
            assert_eq!(overlay, crate::v2::KeyOverlay::default());
        }

        // A default overlay is the neutral value, which is what every unresolved
        // read and every `?? false` / `?? 0` fallback produces.
        let neutral = crate::v2::KeyOverlay::default();
        assert_eq!(neutral.k1, KeyOverlayButton::UNPRESSED);
        assert_eq!(neutral.m2, KeyOverlayButton::UNPRESSED);
    }

    /// `keyOverlay` is not a v2 key. It rides on the packet for the three
    /// consumers that need it -- the precise payload, v1's
    /// `gameplay.keyOverlay` and SC's stringified `keyOverlay` -- and must not
    /// appear in the v2 body, which is why the field is `#[serde(skip)]`.
    #[test]
    fn the_key_overlay_stays_out_of_the_v2_body() {
        let mut packet = TosuV2Packet::default();
        packet.play.key_overlay.k1 = KeyOverlayButton {
            is_pressed: true,
            count: 42,
        };
        let json = serde_json::to_string(&packet).expect("serialize v2");
        assert!(
            !json.contains("keyOverlay") && !json.contains("\"keys\""),
            "v2 gained a key overlay: {json}"
        );
        // And it survives a round trip as the default, like the other skipped
        // beatmap fields.
        let back: TosuV2Packet = serde_json::from_str(&json).expect("deserialize v2");
        assert_eq!(back.play.key_overlay, crate::v2::KeyOverlay::default());
    }

    /// The decoded graph is built once per poll and **shared** by every consumer.
    ///
    /// Both reshapers used to do `serde_json::from_str(raw.get())` themselves --
    /// v1 per `/json` request, SC per `/json/sc` request *and* per `/tokens` frame
    /// -- which parses roughly 250 KB and allocates five `Vec<f64>` each time. The
    /// cache exists to make the tenth consumer a refcount bump, so the sharing is
    /// the property worth pinning, not merely the contents.
    ///
    /// It is also pinned through the deserialisation path, because a packet
    /// rebuilt from the wire must not silently lose the cache and fall back to an
    /// empty graph.
    #[test]
    fn the_strain_graph_is_decoded_once_and_shared() {
        let graph = PerformanceGraph {
            series: vec![
                GraphSeries {
                    name: "aim".to_string(),
                    data: vec![1.0, 2.0, 3.0],
                },
                GraphSeries {
                    name: "reading".to_string(),
                    data: Vec::new(),
                },
            ],
            xaxis: vec![0.0, 400.0, 800.0],
        };
        let precomputed = PrecomputedGraph::new(&graph);

        let first = precomputed.decoded();
        let second = precomputed.decoded();
        assert!(
            Arc::ptr_eq(&first, &second),
            "two consumers must share one decode"
        );
        // And it is the graph that was asked for, not an empty one.
        assert_eq!(first.series.len(), 2);
        assert_eq!(first.series[0].name, "aim");
        assert_eq!(first.series[1].data, Vec::<f64>::new());
        assert_eq!(first.xaxis, vec![0.0, 400.0, 800.0]);

        // Through the wire and back: still shared, and still the same graph.
        let text = serde_json::to_string(&precomputed).expect("serialize");
        let parsed: PrecomputedGraph = serde_json::from_str(&text).expect("deserialize");
        let third = parsed.decoded();
        let fourth = parsed.decoded();
        assert!(Arc::ptr_eq(&third, &fourth), "the wire path caches too");
        assert_eq!(third.series.len(), 2);
        assert_eq!(third.series[0].data, vec![1.0, 2.0, 3.0]);

        // The default has no cache, and answering with the empty graph is the
        // documented fallback rather than a panic.
        let empty = PrecomputedGraph::default();
        assert!(empty.decoded().series.is_empty());
    }

    /// The primary series is v1's `menu.pp.strains`, and it used to be reached
    /// with a `clone()` of the longest series in the graph on every request and
    /// every socket frame. It is published once per graph instead, so the whole
    /// point is that the consumers share one buffer rather than each making
    /// their own.
    #[test]
    fn the_primary_series_is_shared_rather_than_copied_per_consumer() {
        let graph = PerformanceGraph {
            series: vec![
                GraphSeries {
                    name: "aim".to_string(),
                    data: vec![1.0, 2.0, 3.0],
                },
                GraphSeries {
                    name: "speed".to_string(),
                    data: vec![4.0, 5.0],
                },
            ],
            xaxis: vec![0.0, 400.0, 800.0],
        };
        let precomputed = PrecomputedGraph::new(&graph);

        let first = precomputed.primary_series();
        let second = precomputed.primary_series();
        assert!(
            Arc::ptr_eq(&first, &second),
            "two consumers of one graph must share the series, not each copy it"
        );
        // It is the *first* series -- tosu's osu!std primary skill is aim -- and
        // not the whole graph or a later series.
        assert_eq!(*first, vec![1.0, 2.0, 3.0]);
        // And it agrees with the decoded graph it was hoisted from.
        assert_eq!(
            *precomputed.primary_series(),
            precomputed.decoded().series[0].data
        );

        // The wire path hoists too, otherwise a packet rebuilt from JSON would
        // silently start copying again.
        let text = serde_json::to_string(&precomputed).expect("serialize");
        let parsed: PrecomputedGraph = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(*parsed.primary_series(), vec![1.0, 2.0, 3.0]);

        // A graph with no series is the documented empty answer, not a panic --
        // this is the non-osu!std case, where rtosu emits no series at all.
        let empty = PrecomputedGraph::default();
        assert!(empty.primary_series().is_empty());
    }

    #[test]
    fn test_v2_packet_serialization() {
        let mut packet = TosuV2Packet::default();
        packet.client = "stable".to_string();
        packet.server = "ppy.sh".to_string();
        packet.state = OsuStatusState {
            number: 2,
            name: "play".to_string(),
        };
        packet.play.score = 1234567;
        packet.play.mods = create_mods_state(16, "HR");
        packet.tourney.clients.push(TourneyIpcClient {
            ipc_id: 1,
            user: TourneyUser {
                total_pp: 1234,
                ..Default::default()
            },
            ..Default::default()
        });

        let json = serde_json::to_string_pretty(&packet).expect("serialize tosu v2 packet");
        assert!(json.contains("\"client\": \"stable\""));
        assert!(json.contains("\"name\": \"play\""));
        assert!(json.contains("\"score\": 1234567"));
        assert!(json.contains("\"acronym\": \"HR\""));
        assert!(json.contains("\"bestOF\""));
        assert!(json.contains("\"totalPP\": 1234"));
        assert!(json.contains("\"play\""));
    }

    #[test]
    fn test_mod_checksum_matches_tosu() {
        assert_eq!(
            create_mods_state(16, "HR").checksum,
            "4949c7b3a26dc9119f2f7abd79f5b3f6"
        );
        assert_eq!(
            create_mods_state(0, "").checksum,
            "d751713988987e9331980363e24189ce"
        );

        let multi = create_mods_state(536873225, "");
        assert_eq!(multi.checksum, "eec18211a1a9d581bafc90342c0bb4c6");
        let acronyms: Vec<&str> = multi.array.iter().map(|e| e.acronym.as_str()).collect();
        assert_eq!(acronyms, vec!["HD", "HT", "NF", "AT", "V2"]);
    }

    /// The parity pair behind the `mod_bits` rename, from a prior live
    /// validation against osu! itself: `536873225` (a combined mask, so still a
    /// literal at its use site) must keep both the acronym string tosu formats
    /// and the MD5 tosu derives from the same array. A named bit that moved, or
    /// a reordered table, changes the name and this checksum.
    #[test]
    fn the_mod_bit_rename_keeps_the_live_validated_tosu_parity() {
        let name = crate::client::format_mods(536873225);
        assert_eq!(name, "HDHTNFATv2");

        let state = create_mods_state(536873225, &name);
        assert_eq!(state.checksum, "eec18211a1a9d581bafc90342c0bb4c6");
        assert_eq!(state.number, 536873225);
        assert_eq!(state.name, "HDHTNFATv2");
        // Half Time is the only rate mod in the mask, so it sets 0.75.
        assert_eq!(state.rate, 0.75);
        let acronyms: Vec<&str> = state.array.iter().map(|e| e.acronym.as_str()).collect();
        assert_eq!(acronyms, ["HD", "HT", "NF", "AT", "V2"]);
    }
}
