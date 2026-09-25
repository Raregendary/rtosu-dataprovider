use crate::beatmap::{BeatmapPpStats, BeatmapSnapshot};
use crate::client::{mod_acronyms, ModEntry};
use crate::pp::LivePpResult;
use crate::tournament::TournamentChatMessage;
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultsScreenState {
    pub name: String,
    pub score: i32,
    pub max_combo: i32,
    pub rank: String,
    pub pp: BeatmapPpStats,
    pub created_at: String,
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
    pub total_pp: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyIpcClient {
    pub ipc_id: usize,
    pub team: String,
    pub user: TourneyUser,
    pub beatmap: BeatmapSnapshot,
    pub gameplay: PlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TourneyRootState {
    pub score_visible: bool,
    pub stars_visible: bool,
    pub ipc_state: i32,
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
    pub beatmap: BeatmapSnapshot,
    pub play: PlayState,
    pub results_screen: ResultsScreenState,
    pub tourney: TourneyRootState,
}

pub fn create_mods_state(mods_num: u32, mods_str: &str) -> ModsState {
    let array = mod_acronyms(mods_num);
    let rate = if (mods_num & 64) != 0 {
        1.5
    } else if (mods_num & 256) != 0 {
        0.75
    } else {
        1.0
    };

    ModsState {
        checksum: format!("{:x}", mods_num),
        number: mods_num,
        name: mods_str.to_string(),
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
        14 => "rankingTagCoop",
        15 => "rankingTeam",
        16 => "beatmapImport",
        17 => "packageScroll",
        18 => "benchmark",
        19 => "tourney",
        20 => "charts",
        _ => "unknown",
    }
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

        let json = serde_json::to_string_pretty(&packet).expect("serialize tosu v2 packet");
        assert!(json.contains("\"client\": \"stable\""));
        assert!(json.contains("\"name\": \"play\""));
        assert!(json.contains("\"score\": 1234567"));
        assert!(json.contains("\"acronym\": \"HR\""));
    }
}
