use crate::client::{GameplayState, LocalProfile, is_tournament_manager_cmd};
use crate::process::{ProcessMemory, list_processes};
use crate::session::{SoloSession, TournamentSession, TournamentSnapshot};
use crate::v2::*;
use anyhow::Result;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OsuReaderMode {
    #[default]
    Auto,
    Solo,
    Tournament,
}

#[derive(Debug, Clone)]
pub struct OsuReaderBuilder {
    tournament_profile: String,
    solo_profile: String,
    pointer_width: usize,
    scan_limit_bytes: usize,
    poll_interval: Duration,
    proc_check_interval: Duration,
    mode: OsuReaderMode,
    enable_pp: bool,
    pub gradual_pp_chunks: usize,
    enable_hit_errors: bool,
    enable_chat: bool,
}

impl Default for OsuReaderBuilder {
    fn default() -> Self {
        Self {
            tournament_profile: "tournament".to_string(),
            solo_profile: "stable".to_string(),
            pointer_width: 4,
            scan_limit_bytes: 128 * 1024 * 1024,
            poll_interval: Duration::from_millis(16),
            proc_check_interval: Duration::from_millis(1500),
            mode: OsuReaderMode::Auto,
            enable_pp: true,
            gradual_pp_chunks: 100,
            enable_hit_errors: true,
            enable_chat: true,
        }
    }
}

impl OsuReaderBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tournament_profile(mut self, profile: impl Into<String>) -> Self {
        self.tournament_profile = profile.into();
        self
    }

    pub fn solo_profile(mut self, profile: impl Into<String>) -> Self {
        self.solo_profile = profile.into();
        self
    }

    pub fn pointer_width(mut self, width: usize) -> Self {
        self.pointer_width = width;
        self
    }

    pub fn opt_pointer_width(mut self, width: Option<usize>) -> Self {
        if let Some(w) = width {
            self.pointer_width = w;
        }
        self
    }

    pub fn scan_limit_bytes(mut self, limit: usize) -> Self {
        self.scan_limit_bytes = limit;
        self
    }

    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn proc_check_interval(mut self, interval: Duration) -> Self {
        self.proc_check_interval = interval;
        self
    }

    pub fn mode(mut self, mode: OsuReaderMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn enable_tournament(mut self, enable: bool) -> Self {
        self.mode = if enable {
            OsuReaderMode::Tournament
        } else {
            OsuReaderMode::Solo
        };
        self
    }

    pub fn enable_pp(mut self, enable: bool) -> Self {
        self.enable_pp = enable;
        self
    }

    pub fn gradual_pp_chunks(mut self, chunks: usize) -> Self {
        self.gradual_pp_chunks = chunks.clamp(1, 250);
        self
    }

    pub fn enable_hit_errors(mut self, enable: bool) -> Self {
        self.enable_hit_errors = enable;
        self
    }

    pub fn enable_chat(mut self, enable: bool) -> Self {
        self.enable_chat = enable;
        self
    }

    pub fn build(self) -> Result<OsuReader> {
        OsuReader::from_builder(self)
    }
}

pub struct OsuReader {
    builder: OsuReaderBuilder,
    solo_session: SoloSession,
    tourney_session: TournamentSession,
    cached_pids: Vec<u32>,
    is_tournament: bool,
    last_proc_check: Instant,
    last_packet: TosuV2Packet,
}

impl OsuReader {
    pub fn builder() -> OsuReaderBuilder {
        OsuReaderBuilder::new()
    }

    pub fn from_builder(builder: OsuReaderBuilder) -> Result<Self> {
        let mut solo_session = SoloSession::new(
            &builder.solo_profile,
            Some(builder.pointer_width),
            builder.scan_limit_bytes,
        )?;
        solo_session.enable_pp = builder.enable_pp;
        solo_session.gradual_pp_chunks = builder.gradual_pp_chunks;
        solo_session.enable_hit_errors = builder.enable_hit_errors;

        let mut tourney_session = TournamentSession::new(
            &builder.tournament_profile,
            Some(builder.pointer_width),
            builder.scan_limit_bytes,
        )?;
        tourney_session.enable_chat = builder.enable_chat;
        tourney_session.enable_pp = builder.enable_pp;
        tourney_session.gradual_pp_chunks = builder.gradual_pp_chunks;
        tourney_session.enable_hit_errors = builder.enable_hit_errors;

        let mut reader = Self {
            builder,
            solo_session,
            tourney_session,
            cached_pids: Vec::new(),
            is_tournament: false,
            last_proc_check: Instant::now() - Duration::from_secs(60),
            last_packet: TosuV2Packet::default(),
        };

        reader.check_processes();
        Ok(reader)
    }

    pub fn poll(&mut self) -> Result<TosuV2Packet> {
        crate::instr_scope!(ReaderPoll);
        let is_solo = matches!(self.builder.mode, OsuReaderMode::Solo);
        let should_check = self.cached_pids.is_empty()
            || (!is_solo && self.last_proc_check.elapsed() >= self.builder.proc_check_interval);
        if should_check {
            self.check_processes();
        }

        if self.cached_pids.is_empty() {
            let mut packet = TosuV2Packet::default();
            packet.client = "none".to_string();
            packet.state.name = "notRunning".to_string();
            self.last_packet = packet.clone();
            return Ok(packet);
        }

        let is_tourney = match self.builder.mode {
            OsuReaderMode::Solo => false,
            OsuReaderMode::Tournament => true,
            OsuReaderMode::Auto => self.is_tournament,
        };

        if is_tourney {
            let snap = self.tourney_session.poll()?;
            let packet = format_tourney_packet(&snap);
            crate::instr_scope!(PacketClone);
            self.last_packet = packet.clone();
            Ok(packet)
        } else {
            let packet = self.solo_session.poll()?;
            if packet.client == "none" {
                self.cached_pids.clear();
            }
            crate::instr_scope!(PacketClone);
            self.last_packet = packet.clone();
            Ok(packet)
        }
    }

    fn check_processes(&mut self) {
        self.last_proc_check = Instant::now();
        let osu_procs = match list_processes(Some("osu!.exe")) {
            Ok(procs) => procs,
            Err(err) => {
                tracing::debug!("process check snapshot failed: {err:#}");
                return;
            }
        };
        let new_pids: Vec<u32> = osu_procs.iter().map(|p| p.pid).collect();
        if new_pids.is_empty() {
            self.cached_pids.clear();
            return;
        }

        if new_pids != self.cached_pids {
            self.cached_pids = new_pids;
            self.is_tournament = self.cached_pids.len() > 1
                || osu_procs.iter().any(|p| {
                    let cmd = ProcessMemory::open(p.pid)
                        .and_then(|m| m.command_line())
                        .unwrap_or_default();
                    cmd.contains("-spectateclient") || is_tournament_manager_cmd(&cmd)
                });
        }
    }

    pub fn is_tournament(&self) -> bool {
        match self.builder.mode {
            OsuReaderMode::Solo => false,
            OsuReaderMode::Tournament => true,
            OsuReaderMode::Auto => self.is_tournament,
        }
    }

    pub fn is_running(&self) -> bool {
        !self.cached_pids.is_empty()
    }

    pub fn last_packet(&self) -> &TosuV2Packet {
        &self.last_packet
    }

    pub fn poll_interval(&self) -> Duration {
        self.builder.poll_interval
    }

    pub fn into_stream(self) -> OsuReaderStream {
        let interval = self.builder.poll_interval;
        OsuReaderStream {
            reader: self,
            interval: tokio::time::interval(interval),
        }
    }
}

pub struct OsuReaderStream {
    reader: OsuReader,
    interval: tokio::time::Interval,
}

impl OsuReaderStream {
    pub async fn next(&mut self) -> Option<TosuV2Packet> {
        self.interval.tick().await;
        Some(self.reader.poll().unwrap_or_else(|_| {
            let mut packet = TosuV2Packet::default();
            packet.client = "none".to_string();
            packet.state.name = "notRunning".to_string();
            packet
        }))
    }

    pub fn reader(&self) -> &OsuReader {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut OsuReader {
        &mut self.reader
    }
}

pub fn format_tourney_packet(snap: &TournamentSnapshot) -> TosuV2Packet {
    let mut packet = TosuV2Packet {
        client: "stable".to_string(),
        server: "ppy.sh".to_string(),
        profile: guest_profile(),
        state: OsuStatusState {
            number: 22,
            name: "tourney".to_string(),
        },
        ..Default::default()
    };

    if let Some(profile) = snap.profile.as_ref() {
        packet.profile = profile_state(profile);
    }
    packet.folders.game = snap.game_folder.clone();
    packet.folders.songs = snap.songs_folder.clone();
    packet.folders.skin = snap.skin_folder.clone();
    packet.direct_path.skin_folder = snap.skin_folder.clone();
    packet.session.play_time = snap.game_time;
    if let Some(beatmap) = snap.beatmap.as_ref() {
        packet.folders.beatmap = beatmap.folder.clone();
        packet.files.beatmap = beatmap.filename.clone();
        packet.files.background = beatmap.background_filename.clone();
        packet.files.audio = beatmap.audio_filename.clone();
        packet.direct_path.beatmap_folder = beatmap.folder.clone();
        packet.direct_path.beatmap_file = join_path(&beatmap.folder, &beatmap.filename);
        packet.direct_path.beatmap_background =
            join_path(&beatmap.folder, &beatmap.background_filename);
        packet.direct_path.beatmap_audio = join_path(&beatmap.folder, &beatmap.audio_filename);
        packet.beatmap = beatmap.clone();
    }

    if let Some(mgr) = &snap.manager {
        packet.tourney.ipc_state = mgr.ipc_state;
        packet.tourney.best_of = mgr.best_of;
        packet.tourney.score_visible = mgr.score_visible;
        packet.tourney.stars_visible = mgr.stars_visible;
        packet.tourney.points.left = mgr.left_stars;
        packet.tourney.points.right = mgr.right_stars;
        packet.tourney.total_score.left = mgr.left_score as i64;
        packet.tourney.total_score.right = mgr.right_score as i64;
        packet.tourney.team.left = mgr.first_team_name.clone();
        packet.tourney.team.right = mgr.second_team_name.clone();
        packet.tourney.chat = mgr.chat.clone();
    }

    for client in &snap.clients {
        let user = client
            .user
            .as_ref()
            .map(|u| TourneyUser {
                id: u.id,
                name: u.name.clone(),
                country: u.country.to_ascii_uppercase(),
                accuracy: u.accuracy as f32,
                ranked_score: u.ranked_score,
                play_count: u.play_count,
                global_rank: u.global_rank,
                total_pp: u.pp,
            })
            .unwrap_or_default();
        let play = gameplay_to_play(client.gameplay.as_ref());
        let beatmap = TourneyClientBeatmap {
            stats: client
                .beatmap
                .as_ref()
                .map(|v| v.stats.clone())
                .unwrap_or_default(),
        };

        packet.tourney.clients.push(TourneyIpcClient {
            ipc_id: client.ipc_id,
            team: client.team.clone(),
            settings: TourneyClientSettings {
                mania: TourneyManiaSettings { scroll_speed: 12 },
            },
            user,
            beatmap,
            play,
        });
    }

    packet
}

pub fn gameplay_to_play(gameplay: Option<&GameplayState>) -> PlayState {
    let Some(g) = gameplay else {
        return PlayState::default();
    };
    PlayState {
        failed: g.player_hp <= 0.0,
        player_name: g.player_name.clone(),
        mode: OsuStatusState {
            number: g.mode,
            name: ruleset_name(g.mode).to_string(),
        },
        score: g.score,
        accuracy: g.accuracy,
        health_bar: HealthBarState {
            normal: g.player_hp / 2.0,
            smooth: g.player_hp_smooth / 2.0,
        },
        hits: HitsState {
            n0: g.hit_miss as i32,
            n50: g.hit_50 as i32,
            n100: g.hit_100 as i32,
            n300: g.hit_300 as i32,
            geki: g.hit_geki as i32,
            katu: g.hit_katu as i32,
            slider_breaks: g.slider_breaks,
            ..Default::default()
        },
        hit_error_array: std::sync::Arc::clone(&g.hit_error_array),
        combo: ComboState {
            current: g.combo as i32,
            max: g.max_combo as i32,
        },
        mods: create_mods_state(g.mods, &g.mods_str),
        rank: RankState {
            current: g.grade.clone(),
            max_this_play: g.grade_max.clone(),
        },
        unstable_rate: g.unstable_rate,
        ..Default::default()
    }
}

pub fn profile_state(profile: &LocalProfile) -> ProfileState {
    ProfileState {
        user_status: OsuStatusState {
            number: profile.raw_login_status,
            name: login_status_name(profile.raw_login_status).to_string(),
        },
        bancho_status: OsuStatusState {
            number: profile.raw_bancho_status,
            name: bancho_status_name(profile.raw_bancho_status).to_string(),
        },
        id: profile.id,
        name: profile.name.clone(),
        mode: OsuStatusState {
            number: profile.play_mode,
            name: ruleset_name(profile.play_mode).to_string(),
        },
        ranked_score: profile.ranked_score,
        level: profile.level as f64,
        accuracy: profile.accuracy,
        pp: profile.performance_points,
        play_count: profile.play_count,
        global_rank: profile.rank,
        country_code: OsuStatusState {
            number: profile.country_code,
            name: country_name(profile.country_code).to_ascii_uppercase(),
        },
        background_colour: format!("{:x}", profile.background_colour),
        matchmaking: None,
    }
}

pub fn guest_profile() -> ProfileState {
    ProfileState {
        user_status: OsuStatusState {
            number: 256,
            name: "guest".to_string(),
        },
        bancho_status: OsuStatusState {
            number: 0,
            name: "idle".to_string(),
        },
        id: -1,
        name: "Guest".to_string(),
        mode: OsuStatusState {
            number: 0,
            name: "osu".to_string(),
        },
        background_colour: "ff010101".to_string(),
        ..Default::default()
    }
}

pub fn join_path(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.to_string()
    } else if file.is_empty() {
        folder.to_string()
    } else {
        format!("{}\\{}", folder, file)
    }
}

pub fn login_status_name(value: i32) -> &'static str {
    match value {
        0 => "reconnecting",
        256 => "guest",
        257 => "recieving_data",
        65537 => "disconnected",
        65793 => "connected",
        _ => "",
    }
}

pub fn bancho_status_name(value: i32) -> &'static str {
    match value {
        0 => "idle",
        1 => "afk",
        2 => "playing",
        3 => "editing",
        4 => "modding",
        5 => "multiplayer",
        6 => "watching",
        7 => "unknown",
        8 => "testing",
        9 => "submitting",
        10 => "paused",
        11 => "lobby",
        12 => "multiplaying",
        13 => "osuDirect",
        _ => "",
    }
}

pub fn ruleset_name(value: i32) -> &'static str {
    match value {
        0 => "osu",
        1 => "taiko",
        2 => "fruits",
        3 => "mania",
        _ => "",
    }
}

pub fn country_name(value: i32) -> &'static str {
    const CODES: &str = "oc eu ad ae af ag ai al am an ao aq ar as at au aw az ba bb bd be bf bg bh bi bj bm bn bo br bs bt bv bw by bz ca cc cd cf cg ch ci ck cl cm cn co cr cu cv cw cx cy cz de dj dk dm do dz ec ee eg eh er es et fi fj fk fm fo fr fx ga gb gd ge gf gh gi gl gm gn gq gr gs gt gu gw gy hk hm hn hr ht hu id ie il in io iq ir is it jm jo jp ke kg kh ki km kn kp kr kw ky kz la lb lc li lk lr ls lt lu lv ly ma mc md mg mh mk ml mm mn mo mq mr ms mt mu mv mw mx my mz na nc ne nf ng ni nl no np nr nu nz om pa pe pf pg ph pk pl pm pn pr ps pt pw py qa re ro ru rw sa sb sc sd se sg sh si sj sk sl sm sn so sr st sv sy sz tc td tf tg th tj tk tm tn to tl tr tt tv tw tz ua ug um us uy uz va vc ve vg vi vn vu wf ws ye yt rs za zm me zw xx a2 o1 ax gg im je bl mf";
    if value < 1 {
        return "";
    }
    CODES
        .split_whitespace()
        .nth((value - 1) as usize)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_defaults() {
        let builder = OsuReaderBuilder::new();
        assert_eq!(builder.tournament_profile, "tournament");
        assert_eq!(builder.solo_profile, "stable");
        assert_eq!(builder.pointer_width, 4);
        assert_eq!(builder.mode, OsuReaderMode::Auto);
    }

    #[test]
    fn test_builder_custom() {
        let builder = OsuReaderBuilder::new()
            .tournament_profile("custom_tourney")
            .solo_profile("lazer")
            .pointer_width(8)
            .mode(OsuReaderMode::Solo)
            .poll_interval(Duration::from_millis(5));

        assert_eq!(builder.tournament_profile, "custom_tourney");
        assert_eq!(builder.solo_profile, "lazer");
        assert_eq!(builder.pointer_width, 8);
        assert_eq!(builder.mode, OsuReaderMode::Solo);
        assert_eq!(builder.poll_interval, Duration::from_millis(5));
    }

    #[test]
    fn test_reader_creation_and_poll() {
        let mut reader = OsuReader::builder().build().expect("Reader build failed");
        let packet = reader.poll().expect("Poll failed");
        // Even if osu is not running, poll() returns an initialized packet gracefully
        assert!(!packet.client.is_empty());
    }

    #[test]
    fn test_helper_lookups() {
        assert_eq!(ruleset_name(0), "osu");
        assert_eq!(ruleset_name(1), "taiko");
        assert_eq!(ruleset_name(2), "fruits");
        assert_eq!(ruleset_name(3), "mania");
        assert_eq!(ruleset_name(99), "");

        assert_eq!(bancho_status_name(2), "playing");
        assert_eq!(bancho_status_name(0), "idle");

        assert_eq!(login_status_name(65793), "connected");

        assert_eq!(country_name(1), "oc");
        assert_eq!(country_name(2), "eu");
        assert_eq!(country_name(0), "");
    }
}
