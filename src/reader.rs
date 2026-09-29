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
    enable_hit_errors: bool,
    enable_chat: bool,
    mod_multipliers: crate::scoring::ModMultipliers,
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
            enable_hit_errors: true,
            enable_chat: true,
            // A library consumer that never sets a table gets the in-game
            // scores, which is the same contract `[scoring]` ships under.
            mod_multipliers: crate::scoring::ModMultipliers::identity(),
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

    pub fn enable_hit_errors(mut self, enable: bool) -> Self {
        self.enable_hit_errors = enable;
        self
    }

    pub fn enable_chat(mut self, enable: bool) -> Self {
        self.enable_chat = enable;
        self
    }

    /// Weight submitted scores by the mods they were played with.
    ///
    /// Takes a parsed table rather than a config path so the crate's
    /// `[scoring]` section and a library consumer's own numbers reach the
    /// reader the same way. [`crate::scoring::ModMultipliers::identity`] is the
    /// "no weighting" value, and is what the builder starts with.
    pub fn mod_multipliers(mut self, multipliers: crate::scoring::ModMultipliers) -> Self {
        self.mod_multipliers = multipliers;
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
        solo_session.enable_hit_errors = builder.enable_hit_errors;
        solo_session.mod_multipliers = builder.mod_multipliers.clone();

        let mut tourney_session = TournamentSession::new(
            &builder.tournament_profile,
            Some(builder.pointer_width),
            builder.scan_limit_bytes,
        )?;
        tourney_session.enable_chat = builder.enable_chat;
        tourney_session.enable_pp = builder.enable_pp;
        tourney_session.enable_hit_errors = builder.enable_hit_errors;
        tourney_session.mod_multipliers = builder.mod_multipliers.clone();

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
            // No fabricated `client: "none"` / `state.name: "notRunning"`
            // markers: neither is a value tosu can emit, and tosu does not
            // answer with a packet at all when there is no game. The
            // not-running contract is transport-level -- `500
            // {"error":"osu is not ready/running"}` on the `/json*` routes,
            // silence on the sockets -- and it is driven by
            // [`OsuReader::is_attached`] rather than by anything in the body.
            let mut packet = TosuV2Packet::default();
            // `OsuStatusState` derives `Default`, so the placeholder arrives with
            // `number: 0, name: ""` -- and `0` is `menu`
            // (`common/enums/osu.ts:13`, where the enum starts at `menu`). Every
            // real read sets both halves from the same number, so this is the one
            // packet where the invariant would not otherwise hold.
            packet.state.name = crate::v2::osu_state_name(packet.state.number).to_string();
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
            let packet = format_tourney_packet(
                &snap,
                self.tourney_session.enable_hit_errors,
                &self.tourney_session.mod_multipliers,
            );
            crate::instr_scope!(PacketClone);
            self.last_packet = packet.clone();
            Ok(packet)
        } else {
            let packet = self.solo_session.poll()?;
            // The session reports attachment directly. This used to sniff
            // `packet.client == "none"`, which only worked because a sentinel had
            // been written into the payload -- so a fabricated string was load
            // bearing for the process cache, and removing the marker without
            // replacing the signal would have left a dead pid cached forever.
            if !self.solo_session.is_attached() {
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

    /// Whether an osu! process is currently attached.
    ///
    /// **This is the not-running signal.** tosu has no packet to serve when no
    /// game is running, so its `/json*` routes answer `500` and its sockets stay
    /// silent; rtosu carries the same fact here and lets the server reproduce
    /// that, instead of putting a made-up `client` or `state.name` in the body
    /// for a consumer to have to recognise.
    pub fn is_attached(&self) -> bool {
        if self.cached_pids.is_empty() {
            return false;
        }
        match self.builder.mode {
            // The tournament session aggregates several clients, so "attached"
            // is a property of the snapshot rather than of one process.
            OsuReaderMode::Tournament | OsuReaderMode::Auto if self.is_tournament => {
                self.tourney_session.client_count() > 0
            }
            _ => self.solo_session.is_attached(),
        }
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
        // A failed poll yields a plain default packet. Like the empty-process
        // case in `poll`, it carries no marker: the stream's consumer asks
        // `reader().is_attached()`.
        Some(self.reader.poll().unwrap_or_default())
    }

    pub fn reader(&self) -> &OsuReader {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut OsuReader {
        &mut self.reader
    }
}

pub fn format_tourney_packet(
    snap: &TournamentSnapshot,
    enable_hit_errors: bool,
    scoring: &crate::scoring::ModMultipliers,
) -> TosuV2Packet {
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
    packet.game.focused = snap.focused;
    packet.performance = snap.performance.clone();
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
                country: u.country.clone(),
                accuracy: u.accuracy as f32,
                ranked_score: u.ranked_score,
                play_count: u.play_count,
                global_rank: u.global_rank,
                total_pp: u.pp,
            })
            .unwrap_or_default();
        let mut play = gameplay_to_play(client.gameplay.as_ref());
        if !enable_hit_errors {
            play.hit_error_array = std::sync::Arc::default();
        }
        // Weighted here rather than inside `gameplay_to_play`, which is a pure
        // mapping shared with the solo path's own reshaping: a table applied in
        // both places would apply twice. `play.mods.number` is the same mod
        // field the reshape read, so the factor matches the mods in the packet.
        play.score = scoring.apply(play.mods.number, play.score);
        // An empty rank means gameplay state could not be read at all. Leave it
        // empty rather than inventing a grade: a fabricated "XH" reports a
        // silver perfect on every client, including ones that are merely idle
        // or unreadable. Overlays decide what to show for an absent grade.
        if let Some(ref pp) = client.pp {
            play.pp = pp.clone();
        }
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

    apply_weighted_total_score(&mut packet, scoring);

    packet
}

/// Replace the manager's team totals with the sum of the weighted client
/// scores, when there is any weighting to apply.
///
/// **Why this is not just leave-it-alone.** `tourney.totalScore` is the
/// tournament manager's own number, and the manager sums the *unweighted*
/// per-client scores -- it has no idea what factor this provider applied. Once
/// one client carries a factor other than 1.0, the two disagree, and an overlay
/// that draws the total bar beside per-player rows shows a bar that does not
/// equal its own rows. So the total is recomputed here from what the packet
/// actually reports.
///
/// Three cases keep the manager's value instead, each deliberate:
///
/// * an identity table (the feature off, or every factor at 1.0), where the
///   recomputed sum would be the same number;
/// * a packet whose clients are all still at 0, which is what a manager looks
///   like between maps, where its own total is the only real one;
/// * a client whose team is neither `left` nor `right`, which
///   `TournamentSession` does not produce -- dropping it is the honest answer,
///   and it can only ever lose a client the packet could not attribute anyway.
fn apply_weighted_total_score(packet: &mut TosuV2Packet, scoring: &crate::scoring::ModMultipliers) {
    if scoring.is_identity() {
        return;
    }

    let mut left: i64 = 0;
    let mut right: i64 = 0;
    for client in &packet.tourney.clients {
        if client.team == "left" {
            left += i64::from(client.play.score);
        } else if client.team == "right" {
            right += i64::from(client.play.score);
        }
    }
    if left == 0 && right == 0 {
        return;
    }

    if packet.tourney.total_score.left != left || packet.tourney.total_score.right != right {
        tracing::debug!(
            "tourney.totalScore recomputed from weighted client scores: {} (left) / {} (right)",
            left,
            right
        );
    }
    packet.tourney.total_score.left = left;
    packet.tourney.total_score.right = right;
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
        // Carried here as well as on the solo path. The precise payload reads
        // `tourney[].keys` off each client's `play`, and without this the whole
        // per-client key state was the neutral overlay in tournament mode while
        // the same read was honoured one path over -- so a precise frame
        // reported a client's live `hitErrors` beside an all-zero `keys`.
        key_overlay: g.key_overlay,
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

/// The osu! country code table, index 0 = id 1.
///
/// **Transcribed from `tosu-sourcecode/packages/common/enums/country.ts`, which
/// has 252 contiguous members `oc = 1` … `mf = 252`. Do not hand-edit this
/// literal.** The previous version was a whitespace-split string that had
/// picked up a `cw` between `cv` and `cx` (osu! has no `cw` member -- it goes
/// `cv = 52, cx = 53`) and had lost `mp` between `mo = 143` and `mq = 145`, so
/// **143 of 252 ids resolved to the wrong country**: `53` answered `CW`, `86`
/// answered `GN`, `144` answered `MQ`, `225` (the United States) answered `UY`,
/// and `251` answered `MF`. Every player outside ids 1-52 and 87-143 was given
/// somebody else's flag.
///
/// The regeneration is mechanical, so re-derive rather than repair:
///
/// ```text
/// # from the repository root, with the tosu checkout present
/// grep -oE '^\s+\w+ = [0-9]+' tosu-sourcecode/packages/common/enums/country.ts
/// ```
///
/// The type is a fixed-size array rather than a split string so a wrong length
/// is a compile error instead of a silent off-by-one at the tail, and so the
/// one-based id maps to a checked index.
const COUNTRY_CODES: [&str; 252] = [
    "oc", "eu", "ad", "ae", "af", "ag", "ai", "al", "am", "an", "ao", "aq", "ar", "as", "at", "au",
    "aw", "az", "ba", "bb", "bd", "be", "bf", "bg", "bh", "bi", "bj", "bm", "bn", "bo", "br", "bs",
    "bt", "bv", "bw", "by", "bz", "ca", "cc", "cd", "cf", "cg", "ch", "ci", "ck", "cl", "cm", "cn",
    "co", "cr", "cu", "cv", "cx", "cy", "cz", "de", "dj", "dk", "dm", "do", "dz", "ec", "ee", "eg",
    "eh", "er", "es", "et", "fi", "fj", "fk", "fm", "fo", "fr", "fx", "ga", "gb", "gd", "ge", "gf",
    "gh", "gi", "gl", "gm", "gn", "gp", "gq", "gr", "gs", "gt", "gu", "gw", "gy", "hk", "hm", "hn",
    "hr", "ht", "hu", "id", "ie", "il", "in", "io", "iq", "ir", "is", "it", "jm", "jo", "jp", "ke",
    "kg", "kh", "ki", "km", "kn", "kp", "kr", "kw", "ky", "kz", "la", "lb", "lc", "li", "lk", "lr",
    "ls", "lt", "lu", "lv", "ly", "ma", "mc", "md", "mg", "mh", "mk", "ml", "mm", "mn", "mo", "mp",
    "mq", "mr", "ms", "mt", "mu", "mv", "mw", "mx", "my", "mz", "na", "nc", "ne", "nf", "ng", "ni",
    "nl", "no", "np", "nr", "nu", "nz", "om", "pa", "pe", "pf", "pg", "ph", "pk", "pl", "pm", "pn",
    "pr", "ps", "pt", "pw", "py", "qa", "re", "ro", "ru", "rw", "sa", "sb", "sc", "sd", "se", "sg",
    "sh", "si", "sj", "sk", "sl", "sm", "sn", "so", "sr", "st", "sv", "sy", "sz", "tc", "td", "tf",
    "tg", "th", "tj", "tk", "tm", "tn", "to", "tl", "tr", "tt", "tv", "tw", "tz", "ua", "ug", "um",
    "us", "uy", "uz", "va", "vc", "ve", "vg", "vi", "vn", "vu", "wf", "ws", "ye", "yt", "rs", "za",
    "zm", "me", "zw", "xx", "a2", "o1", "ax", "gg", "im", "je", "bl", "mf",
];

/// The country code for an osu! country id, or `""` outside 1..=252.
///
/// tosu emits the code **uppercased** at the payload sites
/// (`api/utils/buildResultV2.ts:319-322`), so this returns the lowercase enum
/// name and the callers uppercase it. The empty string for an out-of-range id
/// is tosu's own behaviour: `CountryCodes[value]` on a numeric enum is
/// `undefined`, and `JSON.stringify` drops an undefined value, so the key
/// disappears from the object rather than becoming `null`.
pub fn country_name(value: i32) -> &'static str {
    // `checked_sub` first, not `value - 1`: the conversion below would be handed
    // an already-overflowed `i32` for `i32::MIN`, and arithmetic overflow panics
    // in a debug build. The country id comes straight out of the target process,
    // so every value in `i32` has to be answerable without panicking.
    value
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| COUNTRY_CODES.get(index).copied())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `gameplay_to_play` must carry the key overlay, not just the counters.
    ///
    /// The precise payload reads `tourney[].keys` off each client's `play`, and
    /// this reshape ended in `..Default::default()` without assigning it. So in
    /// tournament mode every precise frame reported a client's live `hitErrors`
    /// next to an all-zero `keys` -- the same read honoured on the solo path and
    /// dropped here, which is exactly the shape a "the read exists" test cannot
    /// see.
    #[test]
    fn the_tournament_reshape_carries_the_key_overlay() {
        let mut gameplay = crate::client::GameplayState::default();
        gameplay.key_overlay = crate::v2::KeyOverlay {
            k1: crate::v2::KeyOverlayButton {
                is_pressed: true,
                count: 11,
            },
            k2: crate::v2::KeyOverlayButton {
                is_pressed: false,
                count: 9,
            },
            m1: crate::v2::KeyOverlayButton::default(),
            m2: crate::v2::KeyOverlayButton::default(),
        };

        let play = gameplay_to_play(Some(&gameplay));

        assert_eq!(play.key_overlay, gameplay.key_overlay);
        assert_eq!(
            play.key_overlay.k1.count, 11,
            "the count reaches the client"
        );
        assert!(play.key_overlay.k1.is_pressed);

        // And the neutral case still round-trips, rather than the field being
        // left in a state that serialises as something other than four buttons.
        assert_eq!(
            gameplay_to_play(None).key_overlay,
            crate::v2::KeyOverlay::default(),
            "no gameplay means the neutral overlay"
        );
    }

    /// The poll loop does synchronous memory scans, disk reads and thread
    /// spawning, so keeping it off the async worker is only possible if the
    /// reader can be moved to a thread of its own. `ProcessMemory` declares
    /// `unsafe impl Send + Sync` explicitly; this asserts the property actually
    /// holds for the whole reader, so a future field that is not `Send` fails
    /// here rather than in the middle of a refactor.
    #[test]
    fn the_reader_can_be_moved_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<OsuReader>();
    }

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

    /// `poll()` succeeds when no osu! is running, and reports that fact through
    /// `is_attached()` rather than through the payload.
    ///
    /// This test used to assert `!packet.client.is_empty()`, which was only
    /// checking that the invented `client: "none"` marker had been written --
    /// it had nothing to do with reading a game. The marker is gone: `client` is
    /// `ClientType[game.client]` upstream, so `"none"` is a value no tosu build
    /// emits for a running game and a value that means "no game" only inside
    /// rtosu. tosu does not answer with a packet at all in this state, it answers
    /// `500` -- so the signal belongs on the reader, where `main.rs` forwards it
    /// as `PublishedPacket::attached`.
    #[test]
    fn poll_succeeds_without_a_game_and_reports_it_on_the_reader() {
        let mut reader = OsuReader::builder().build().expect("Reader build failed");
        let packet = reader.poll().expect("Poll failed");

        assert!(
            !reader.is_attached(),
            "no osu! process in this test, so nothing is attached"
        );
        // No fabricated marker anywhere in the body.
        assert_ne!(packet.client, "none", "no sentinel in the payload");
        assert_ne!(
            packet.state.name, "notRunning",
            "no sentinel in the payload"
        );
        // And the packet is a real, if empty, one.
        assert_eq!(packet.state.number, 0);
        assert_eq!(
            crate::v2::osu_state_name(packet.state.number),
            packet.state.name,
            "state.name follows the number, as it does everywhere else"
        );
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

    /// The country table, swept across its whole domain against a **separately
    /// transcribed** copy of `common/enums/country.ts`.
    ///
    /// The two literals are the same data, which is the point: asserting the
    /// table against itself proves nothing, and asserting it against rtosu's
    /// *old* output would just pin the bug. The old table had 251 entries with a
    /// `cw` that osu! does not have and no `mp`, so **143 of 252 ids** answered
    /// with the wrong country -- including `225`, the United States. A sweep is
    /// what catches that class of damage, because the head (`1..=52`) agreed
    /// throughout and so did every spot check anyone would have reached for
    /// first.
    ///
    /// The id boundaries are asserted by name as well, because those are the
    /// entries the drift moved and the ones a reader can check against tosu's
    /// own output without diffing 252 rows.
    #[test]
    fn every_country_id_resolves_to_tosus_code() {
        /// Transcribed from `tosu-sourcecode/packages/common/enums/country.ts`:
        /// 252 contiguous members, `oc = 1` … `mf = 252`.
        const EXPECTED: [&str; 252] = [
            "oc", "eu", "ad", "ae", "af", "ag", "ai", "al", "am", "an", "ao", "aq", "ar", "as",
            "at", "au", "aw", "az", "ba", "bb", "bd", "be", "bf", "bg", "bh", "bi", "bj", "bm",
            "bn", "bo", "br", "bs", "bt", "bv", "bw", "by", "bz", "ca", "cc", "cd", "cf", "cg",
            "ch", "ci", "ck", "cl", "cm", "cn", "co", "cr", "cu", "cv", "cx", "cy", "cz", "de",
            "dj", "dk", "dm", "do", "dz", "ec", "ee", "eg", "eh", "er", "es", "et", "fi", "fj",
            "fk", "fm", "fo", "fr", "fx", "ga", "gb", "gd", "ge", "gf", "gh", "gi", "gl", "gm",
            "gn", "gp", "gq", "gr", "gs", "gt", "gu", "gw", "gy", "hk", "hm", "hn", "hr", "ht",
            "hu", "id", "ie", "il", "in", "io", "iq", "ir", "is", "it", "jm", "jo", "jp", "ke",
            "kg", "kh", "ki", "km", "kn", "kp", "kr", "kw", "ky", "kz", "la", "lb", "lc", "li",
            "lk", "lr", "ls", "lt", "lu", "lv", "ly", "ma", "mc", "md", "mg", "mh", "mk", "ml",
            "mm", "mn", "mo", "mp", "mq", "mr", "ms", "mt", "mu", "mv", "mw", "mx", "my", "mz",
            "na", "nc", "ne", "nf", "ng", "ni", "nl", "no", "np", "nr", "nu", "nz", "om", "pa",
            "pe", "pf", "pg", "ph", "pk", "pl", "pm", "pn", "pr", "ps", "pt", "pw", "py", "qa",
            "re", "ro", "ru", "rw", "sa", "sb", "sc", "sd", "se", "sg", "sh", "si", "sj", "sk",
            "sl", "sm", "sn", "so", "sr", "st", "sv", "sy", "sz", "tc", "td", "tf", "tg", "th",
            "tj", "tk", "tm", "tn", "to", "tl", "tr", "tt", "tv", "tw", "tz", "ua", "ug", "um",
            "us", "uy", "uz", "va", "vc", "ve", "vg", "vi", "vn", "vu", "wf", "ws", "ye", "yt",
            "rs", "za", "zm", "me", "zw", "xx", "a2", "o1", "ax", "gg", "im", "je", "bl", "mf",
        ];

        for (index, expected) in EXPECTED.iter().enumerate() {
            let id = index as i32 + 1;
            assert_eq!(
                country_name(id),
                *expected,
                "country id {id} must be {expected}"
            );
        }

        // The boundaries the drift actually moved, named rather than swept.
        assert_eq!(country_name(52), "cv", "last id before the missing cw");
        assert_eq!(country_name(53), "cx", "osu! has no cw member");
        assert_eq!(country_name(86), "gp", "last id before the missing mp");
        assert_eq!(country_name(143), "mo", "last id before mp");
        assert_eq!(country_name(144), "mp", "mp is a real member tosu has");
        assert_eq!(country_name(225), "us", "the United States");
        assert_eq!(country_name(251), "bl", "second to last member");
        assert_eq!(country_name(252), "mf", "last member, id 252");

        // Outside the table there is no code at all, and tosu drops the key
        // rather than emitting null: `CountryCodes[value]?.toUpperCase() || ''`
        // (`buildResultV2.ts:319-322`).
        for out_of_range in [0, -1, 253, 1000, i32::MAX, i32::MIN] {
            assert_eq!(country_name(out_of_range), "", "id {out_of_range}");
        }
    }

    /// A tourney client with a readable gameplay state, so the reshape has
    /// something to weight.
    fn tourney_client(
        ipc_id: usize,
        team: &str,
        mods: u32,
        score: i32,
    ) -> crate::session::TournamentClientView {
        let mut gameplay = crate::client::GameplayState::default();
        gameplay.mods = mods;
        gameplay.mods_str = crate::client::format_mods(mods);
        gameplay.score = score;
        crate::session::TournamentClientView {
            pid: 0,
            ipc_id,
            team: team.to_string(),
            user: None,
            gameplay: Some(gameplay),
            beatmap: None,
            pp: None,
            error: None,
        }
    }

    /// The manager's read, as the tournament session would have read it: the
    /// sum of the *unweighted* client scores.
    fn manager(left_score: i32, right_score: i32) -> crate::tournament::TournamentState {
        crate::tournament::TournamentState {
            ruleset_address: 0,
            left_team_address: 0,
            right_team_address: 0,
            ipc_state: 2,
            is_tourney: true,
            best_of: 0,
            left_score,
            right_score,
            left_stars: 0,
            right_stars: 0,
            first_team_name: String::new(),
            second_team_name: String::new(),
            stars_visible: false,
            score_visible: false,
            finalized: false,
            chat: Vec::new(),
        }
    }

    fn tourney_snapshot(
        clients: Vec<crate::session::TournamentClientView>,
        manager: crate::tournament::TournamentState,
    ) -> crate::session::TournamentSnapshot {
        crate::session::TournamentSnapshot {
            captured_at_ms: 0,
            poll_duration_us: 0,
            manager: Some(manager),
            profile: None,
            beatmap: None,
            game_folder: String::new(),
            songs_folder: String::new(),
            skin_folder: String::new(),
            game_time: 0,
            clients,
            performance: Default::default(),
            focused: false,
        }
    }

    fn multipliers(entries: &[(&str, f64)]) -> crate::scoring::ModMultipliers {
        let raw = entries
            .iter()
            .map(|(key, factor)| (key.to_string(), *factor))
            .collect();
        crate::scoring::ModMultipliers::new(&raw).expect("the test table must be valid")
    }

    /// With the feature off, the packet is the one rtosu served before it
    /// existed: the clients' in-game scores and the manager's own totals.
    ///
    /// This is the parity guard. Every tournament overlay that exists today
    /// reads these fields, and the tosu diff runs with the default config, so
    /// the unweighted path has to stay byte-identical.
    #[test]
    fn an_identity_table_leaves_the_tournament_packet_untouched() {
        let clients = vec![
            tourney_client(0, "left", crate::client::mod_bits::EZ, 1_000_000),
            tourney_client(1, "right", 0, 2_000_000),
        ];
        let packet = format_tourney_packet(
            &tourney_snapshot(clients, manager(1_000_000, 2_000_000)),
            true,
            &crate::scoring::ModMultipliers::identity(),
        );

        assert_eq!(packet.tourney.clients[0].play.score, 1_000_000);
        assert_eq!(packet.tourney.clients[1].play.score, 2_000_000);
        assert_eq!(
            packet.tourney.total_score.left, 1_000_000,
            "the manager's read is already the right answer with no weighting"
        );
        assert_eq!(packet.tourney.total_score.right, 2_000_000);
    }

    /// With the feature on, each client's score is weighted exactly once and
    /// `totalScore` becomes the sum of those weighted scores, because the
    /// manager's own total cannot be rescaled per team: two clients on the same
    /// team can carry different mods.
    #[test]
    fn weighted_client_scores_are_summed_into_the_team_totals() {
        let clients = vec![
            tourney_client(0, "left", crate::client::mod_bits::EZ, 1_000_000),
            tourney_client(1, "left", 0, 500_000),
            tourney_client(
                2,
                "right",
                crate::client::mod_bits::DT | crate::client::mod_bits::NC,
                2_000_000,
            ),
        ];
        let packet = format_tourney_packet(
            &tourney_snapshot(clients, manager(1_500_000, 2_000_000)),
            true,
            &multipliers(&[("EZ", 1.8), ("NM", 1.0), ("DT/NC", 1.5)]),
        );

        assert_eq!(
            packet.tourney.clients[0].play.score, 1_800_000,
            "1.8x, applied once"
        );
        assert_eq!(packet.tourney.clients[1].play.score, 500_000);
        assert_eq!(
            packet.tourney.clients[2].play.score, 3_000_000,
            "Nightcore is one slot, so 1.5x rather than 2.25x"
        );

        assert_eq!(packet.tourney.total_score.left, 2_300_000);
        assert_eq!(
            packet.tourney.total_score.right, 3_000_000,
            "the manager's 2_000_000 is not what the packet reports per client"
        );
    }

    /// A scoreline that has not started yet keeps the manager's read, so the
    /// score bar does not blank out during a map load or a retry.
    #[test]
    fn an_all_zero_scoreline_keeps_the_managers_totals() {
        let clients = vec![
            tourney_client(0, "left", crate::client::mod_bits::EZ, 0),
            tourney_client(1, "right", crate::client::mod_bits::EZ, 0),
        ];
        let packet = format_tourney_packet(
            &tourney_snapshot(clients, manager(3_000_000, 4_000_000)),
            true,
            &multipliers(&[("EZ", 1.8)]),
        );

        assert_eq!(packet.tourney.total_score.left, 3_000_000);
        assert_eq!(packet.tourney.total_score.right, 4_000_000);
    }

    /// A client whose gameplay state could not be read contributes its 0 like
    /// any other client, so an unreadable client cannot make a weighted team
    /// total disagree with the rows printed beside it.
    #[test]
    fn an_unreadable_client_contributes_zero() {
        let mut unreadable = tourney_client(0, "left", crate::client::mod_bits::EZ, 0);
        unreadable.gameplay = None;
        let clients = vec![unreadable, tourney_client(1, "right", 0, 1_000_000)];
        let packet = format_tourney_packet(
            &tourney_snapshot(clients, manager(0, 1_000_000)),
            true,
            &multipliers(&[("EZ", 1.8)]),
        );

        assert_eq!(packet.tourney.clients[0].play.score, 0);
        assert_eq!(packet.tourney.total_score.left, 0);
        assert_eq!(packet.tourney.total_score.right, 1_000_000);
    }
}
