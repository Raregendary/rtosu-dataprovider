use crate::address::checked_add;
use crate::process::ProcessMemory;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const TEAM_LEFT_POINTER_OFFSET: u64 = 0x1c;
pub const TEAM_RIGHT_POINTER_OFFSET: u64 = 0x20;
pub const IPC_STATE_OFFSET: u64 = 0x54;
pub const TEAM_BEST_OF_OFFSET: u64 = 0x30;
pub const SCORE_OFFSET: u64 = 0x28;
pub const STARS_OFFSET: u64 = 0x2c;
pub const STARS_VISIBLE_OFFSET: u64 = 0x38;
pub const SCORE_VISIBLE_OFFSET: u64 = 0x39;
pub const FINALIZED_OFFSET: u64 = 0x3a;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TournamentChatMessage {
    #[serde(rename = "timestamp")]
    pub time: String,
    pub name: String,
    pub message: String,
    pub team: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TournamentState {
    pub ruleset_address: u64,
    pub left_team_address: u64,
    pub right_team_address: u64,
    pub ipc_state: i32,
    pub is_tourney: bool,
    pub best_of: i32,
    pub left_score: i32,
    pub right_score: i32,
    pub left_stars: i32,
    pub right_stars: i32,
    pub first_team_name: String,
    pub second_team_name: String,
    pub stars_visible: bool,
    pub score_visible: bool,
    pub finalized: bool,
    #[serde(default)]
    pub chat: Vec<TournamentChatMessage>,
}

pub fn read_team_name(memory: &ProcessMemory, team_address: u64) -> String {
    let Ok(inner_ptr) = memory.read_pointer(checked_add(team_address, 0x20).unwrap_or(0)) else {
        return String::new();
    };
    if inner_ptr == 0 {
        return String::new();
    }
    let Ok(name_ptr) = memory.read_pointer(checked_add(inner_ptr, 0x144).unwrap_or(0)) else {
        return String::new();
    };
    if name_ptr == 0 {
        return String::new();
    }
    memory.read_dotnet_string(name_ptr, 128).unwrap_or_default()
}

pub fn read_tournament_state(
    memory: &ProcessMemory,
    ruleset_address: u64,
) -> Result<TournamentState> {
    if ruleset_address == 0 {
        bail!("ruleset address is null");
    }
    let ipc_state = memory
        .read_i32(field(ruleset_address, IPC_STATE_OFFSET)?)
        .context("reading tournament IPC state")?;
    let left_team_address = memory
        .read_pointer(field(ruleset_address, TEAM_LEFT_POINTER_OFFSET)?)
        .unwrap_or(0);
    let right_team_address = memory
        .read_pointer(field(ruleset_address, TEAM_RIGHT_POINTER_OFFSET)?)
        .unwrap_or(0);

    if ipc_state == 0 && left_team_address == 0 && right_team_address == 0 {
        bail!("process is not a tournament manager");
    }

    let (left_score, left_stars, first_team_name, left_finalized) = if left_team_address != 0 {
        (
            memory
                .read_i32(field(left_team_address, SCORE_OFFSET)?)
                .unwrap_or(0),
            memory
                .read_i32(field(left_team_address, STARS_OFFSET)?)
                .unwrap_or(0),
            read_team_name(memory, left_team_address),
            memory
                .read_u8(field(left_team_address, FINALIZED_OFFSET)?)
                .unwrap_or(0)
                != 0,
        )
    } else {
        (0, 0, String::new(), false)
    };

    let (
        right_score,
        right_stars,
        second_team_name,
        right_stars_visible,
        right_score_visible,
        right_finalized,
        best_of,
    ) = if right_team_address != 0 {
        (
            memory
                .read_i32(field(right_team_address, SCORE_OFFSET)?)
                .unwrap_or(0),
            memory
                .read_i32(field(right_team_address, STARS_OFFSET)?)
                .unwrap_or(0),
            read_team_name(memory, right_team_address),
            memory
                .read_u8(field(right_team_address, STARS_VISIBLE_OFFSET)?)
                .unwrap_or(0)
                != 0,
            memory
                .read_u8(field(right_team_address, SCORE_VISIBLE_OFFSET)?)
                .unwrap_or(0)
                != 0,
            memory
                .read_u8(field(right_team_address, FINALIZED_OFFSET)?)
                .unwrap_or(0)
                != 0,
            memory
                .read_i32(field(right_team_address, TEAM_BEST_OF_OFFSET)?)
                .unwrap_or(0),
        )
    } else {
        (0, 0, String::new(), false, false, false, 0)
    };

    Ok(TournamentState {
        ruleset_address,
        left_team_address,
        right_team_address,
        ipc_state,
        is_tourney: ipc_state & 2 != 0,
        best_of,
        left_score,
        right_score,
        left_stars,
        right_stars,
        first_team_name,
        second_team_name,
        stars_visible: right_stars_visible,
        score_visible: right_score_visible,
        finalized: left_finalized || right_finalized,
        chat: Vec::new(),
    })
}

pub fn read_tournament_chat(
    memory: &ProcessMemory,
    chat_engine_pattern_addr: u64,
    spectator_teams: &HashMap<String, String>,
) -> Result<Vec<TournamentChatMessage>> {
    if chat_engine_pattern_addr == 0 {
        return Ok(Vec::new());
    }
    let channels_list_static_ptr = memory.read_pointer(chat_engine_pattern_addr)?;
    if channels_list_static_ptr == 0 {
        return Ok(Vec::new());
    }
    let channels_list = memory.read_pointer(channels_list_static_ptr)?;
    if channels_list == 0 {
        return Ok(Vec::new());
    }
    let channels_items = memory.read_pointer(checked_add(channels_list, 0x4)?)?;
    if channels_items == 0 {
        return Ok(Vec::new());
    }
    let channels_len = memory.read_i32(checked_add(channels_items, 0x4)?)?;
    if channels_len <= 0 || channels_len > 1024 {
        return Ok(Vec::new());
    }

    let mut messages = Vec::new();
    for i in (0..channels_len).rev() {
        let channel_slot = checked_add(channels_items, (8 + 4 * i) as u64)?;
        let channel_addr = match memory.read_pointer(channel_slot) {
            Ok(addr) if addr != 0 => addr,
            _ => continue,
        };
        let tag_ptr = match memory.read_pointer(checked_add(channel_addr, 0x4)?) {
            Ok(ptr) if ptr != 0 => ptr,
            _ => continue,
        };
        let tag = match memory.read_dotnet_string(tag_ptr, 32) {
            Ok(t) => t,
            _ => continue,
        };
        if tag != "#multiplayer" {
            continue;
        }

        let messages_list = match memory.read_pointer(checked_add(channel_addr, 0x10)?) {
            Ok(addr) if addr != 0 => addr,
            _ => continue,
        };
        let messages_items = match memory.read_pointer(checked_add(messages_list, 0x4)?) {
            Ok(addr) if addr != 0 => addr,
            _ => continue,
        };
        let messages_size = match memory.read_i32(checked_add(messages_list, 0xc)?) {
            Ok(size) if size >= 0 => size.min(500) as usize,
            _ => continue,
        };

        for m in 0..messages_size {
            let msg_slot = match checked_add(messages_items, (8 + 4 * m) as u64) {
                Ok(slot) => slot,
                Err(_) => continue,
            };
            let msg_ptr = match memory.read_pointer(msg_slot) {
                Ok(ptr) if ptr != 0 => ptr,
                _ => continue,
            };
            let content_ptr = match memory.read_pointer(checked_add(msg_ptr, 0x4)?) {
                Ok(ptr) if ptr != 0 => ptr,
                _ => continue,
            };
            let content = match memory.read_dotnet_string(content_ptr, 512) {
                Ok(c) if !c.is_empty() => c,
                _ => continue,
            };
            let time_name_ptr = match memory.read_pointer(checked_add(msg_ptr, 0x8)?) {
                Ok(ptr) if ptr != 0 => ptr,
                _ => continue,
            };
            let time_name = memory
                .read_dotnet_string(time_name_ptr, 128)
                .unwrap_or_default();
            let mut parts = time_name.splitn(2, ' ');
            let time = parts.next().unwrap_or("").trim().to_string();
            let raw_author = parts.next().unwrap_or("").trim();
            let author = raw_author.trim_end_matches(':').trim().to_string();

            let team = if author == "BanchoBot" {
                "bot".to_string()
            } else if let Some(t) = spectator_teams.get(&author) {
                t.clone()
            } else {
                "unknown".to_string()
            };

            messages.push(TournamentChatMessage {
                time,
                name: author,
                message: content,
                team,
            });
        }
        break;
    }

    Ok(messages)
}

fn field(address: u64, offset: u64) -> Result<u64> {
    checked_add(address, offset)
}

#[cfg(test)]
mod tests {
    use super::{IPC_STATE_OFFSET, TEAM_LEFT_POINTER_OFFSET, TEAM_RIGHT_POINTER_OFFSET};

    #[test]
    fn keeps_known_tournament_layout_offsets() {
        assert_eq!(TEAM_LEFT_POINTER_OFFSET, 0x1c);
        assert_eq!(TEAM_RIGHT_POINTER_OFFSET, 0x20);
        assert_eq!(IPC_STATE_OFFSET, 0x54);
    }
}
