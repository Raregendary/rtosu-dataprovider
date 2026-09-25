use crate::beatmap::{BeatmapSnapshot, BeatmapStats};
use crate::client::{ModEntry, mod_acronyms};
use crate::pp::LivePpResult;
use crate::tournament::TournamentChatMessage;
use md5::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModsState {
    pub checksum: String,
    pub number: u32,
    pub name: String,
    pub array: Vec<ModEntry>,
    pub rate: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RankState {
    pub current: String,
    pub max_this_play: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayState {
    pub failed: bool,
    pub player_name: String,
    pub mode: OsuStatusState,
    pub score: i32,
    pub accuracy: f64,
    pub health_bar: HealthBarState,
    pub hits: HitsState,
    pub hit_error_array: Vec<i32>,
    pub combo: ComboState,
    pub mods: ModsState,
    pub rank: RankState,
    pub pp: LivePpResult,
    pub unstable_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsScreenPp {
    pub current: f32,
    pub fc: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsScreenState {
    pub score_id: i64,
    pub player_name: String,
    pub name: String,
    pub mode: OsuStatusState,
    pub score: i32,
    pub accuracy: f64,
    pub hits: ResultsHitsState,
    pub mods: ModsState,
    pub max_combo: i32,
    pub rank: String,
    pub pp: ResultsScreenPp,
    pub created_at: String,
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
    pub data: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceGraph {
    pub series: Vec<GraphSeries>,
    pub xaxis: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceState {
    pub accuracy: PerformanceAccuracy,
    pub graph: PerformanceGraph,
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

pub fn create_mods_state(mods_num: u32, mods_str: &str) -> ModsState {
    let array = mod_acronyms(mods_num);
    let rate = if (mods_num & 64) != 0 || (mods_num & 512) != 0 {
        1.5
    } else if (mods_num & 256) != 0 {
        0.75
    } else {
        1.0
    };
    let checksum = if array.is_empty() {
        String::new()
    } else {
        md5_hex(serde_json::to_string(&array).unwrap_or_default().as_bytes())
    };

    let name = crate::client::format_mods(mods_num);
    ModsState {
        checksum,
        number: mods_num,
        name: if name.is_empty() && !mods_str.is_empty() {
            mods_str.to_string()
        } else {
            name
        },
        array,
        rate,
    }
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
        assert_eq!(create_mods_state(0, "").checksum, "");
    }
}
