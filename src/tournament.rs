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

/// Upper bound on chat messages walked per tick.
///
/// osu! lazer bounds channel history at 300 (`ppy/osu`,
/// `osu.Game/Online/Chat/Channel.cs`, `MAX_HISTORY`), so a healthy channel never reaches this.
/// Stable's cap is not documented, so this is a backstop against a corrupt length field,
/// not a behavioural limit.
const MAX_CHAT_MESSAGES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatCacheKey {
    /// Index of the newest parsed message within `_items`.
    pub index: usize,
    /// Address of the newest `ChatMessage` object.
    pub message_ptr: u64,
    /// Address of that message's `content` string.
    pub content_ptr: u64,
    /// `String._length` of that content string (x86 .NET: `content_ptr + 4`).
    pub content_len: i32,
}

impl ChatCacheKey {
    /// Whether `previous` (the key stored alongside the last successful parse) still
    /// describes the live buffer. Pure so it is testable without a live process.
    pub fn is_valid_against(&self, previous: Option<&Self>) -> bool {
        let Some(previous) = previous else {
            return false;
        };
        if self.message_ptr == 0 {
            return false;
        }
        *self == *previous
    }
}

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

#[derive(Debug, Clone, Default)]
pub struct TournamentChat {
    /// Fingerprint of the newest message; the caller stores this as its cache key.
    pub key: ChatCacheKey,
    pub messages: Vec<TournamentChatMessage>,
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
    cached_chat: Option<(&ChatCacheKey, &[TournamentChatMessage])>,
) -> Result<TournamentChat> {
    crate::instr_scope!(TournamentChat);
    if chat_engine_pattern_addr == 0 {
        return Ok(TournamentChat::default());
    }
    let channels_list_static_ptr = memory.read_pointer(chat_engine_pattern_addr)?;
    if channels_list_static_ptr == 0 {
        return Ok(TournamentChat::default());
    }
    let channels_list = memory.read_pointer(channels_list_static_ptr)?;
    if channels_list == 0 {
        return Ok(TournamentChat::default());
    }
    let channels_items = memory.read_pointer(checked_add(channels_list, 0x4)?)?;
    if channels_items == 0 {
        return Ok(TournamentChat::default());
    }
    let channels_len = memory.read_i32(checked_add(channels_items, 0x4)?)?;
    if channels_len <= 0 || channels_len > 1024 {
        return Ok(TournamentChat::default());
    }

    let mut messages = Vec::new();
    let mut key = ChatCacheKey::default();
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
        // The array's own length, not `List._size`/`List._version` at `messages_list + 0xc`:
        // the array length is a property of the array object and is unambiguous, whereas that
        // field cannot be identified without a live process to dump. osu! evicts from the front
        // of a full buffer, so the count alone cannot detect change (see `ChatCacheKey`).
        let capacity = match memory.read_i32(checked_add(messages_items, 0x4)?) {
            Ok(len) => match chat_capacity(len) {
                Some(capacity) => capacity,
                None => continue,
            },
            Err(_) => continue,
        };

        if let Some((previous, cached_msgs)) = cached_chat
            && !cached_msgs.is_empty()
            && previous.index < capacity
            && let Some(live) = fingerprint_chat_slot(memory, messages_items, previous.index)
            && live.is_valid_against(Some(previous))
        {
            return Ok(TournamentChat {
                key: live,
                messages: cached_msgs.to_vec(),
            });
        }

        for m in 0..capacity {
            let msg_slot = match checked_add(messages_items, (8 + 4 * m) as u64) {
                Ok(slot) => slot,
                Err(_) => continue,
            };
            // A null slot ends the used region: `List<T>` leaves the tail of `_items` nulled
            // and used slots are contiguous from index 0, so the walk must stop here.
            let msg_ptr = match memory.read_pointer(msg_slot) {
                Ok(ptr) if ptr != 0 => ptr,
                _ => break,
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
            key = ChatCacheKey {
                index: m,
                message_ptr: msg_ptr,
                content_ptr,
                content_len: read_string_length(memory, content_ptr),
            };
        }
        break;
    }

    Ok(TournamentChat { key, messages })
}

/// The `List<T>` array length, accepted only when it is a plausible walk bound.
fn chat_capacity(len: i32) -> Option<usize> {
    if len > 0 && len as usize <= MAX_CHAT_MESSAGES {
        Some(len as usize)
    } else {
        None
    }
}

/// Fingerprint the message at `index`, or `None` when the slot holds nothing usable.
fn fingerprint_chat_slot(
    memory: &ProcessMemory,
    messages_items: u64,
    index: usize,
) -> Option<ChatCacheKey> {
    let slot = checked_add(messages_items, (8 + 4 * index) as u64).ok()?;
    let message_ptr = memory.read_pointer(slot).ok()?;
    if message_ptr == 0 {
        return None;
    }
    let content_ptr = memory
        .read_pointer(checked_add(message_ptr, 0x4).ok()?)
        .unwrap_or(0);
    Some(ChatCacheKey {
        index,
        message_ptr,
        content_ptr,
        content_len: read_string_length(memory, content_ptr),
    })
}

/// `String._length`, using the same field offset as `ProcessMemory::read_dotnet_string`.
fn read_string_length(memory: &ProcessMemory, string_ptr: u64) -> i32 {
    if string_ptr == 0 {
        return 0;
    }
    let length_offset = if memory.pointer_size() == 4 { 4 } else { 8 };
    match checked_add(string_ptr, length_offset as u64) {
        Ok(address) => memory.read_i32(address).unwrap_or_default(),
        Err(_) => 0,
    }
}

fn field(address: u64, offset: u64) -> Result<u64> {
    checked_add(address, offset)
}

#[cfg(test)]
mod tests {
    use super::{
        ChatCacheKey, IPC_STATE_OFFSET, MAX_CHAT_MESSAGES, TEAM_LEFT_POINTER_OFFSET,
        TEAM_RIGHT_POINTER_OFFSET, chat_capacity,
    };

    fn key(index: usize, message_ptr: u64, content_ptr: u64, content_len: i32) -> ChatCacheKey {
        ChatCacheKey {
            index,
            message_ptr,
            content_ptr,
            content_len,
        }
    }

    #[test]
    fn keeps_known_tournament_layout_offsets() {
        assert_eq!(TEAM_LEFT_POINTER_OFFSET, 0x1c);
        assert_eq!(TEAM_RIGHT_POINTER_OFFSET, 0x20);
        assert_eq!(IPC_STATE_OFFSET, 0x54);
    }

    #[test]
    fn unchanged_fingerprint_reuses_cached_chat() {
        let previous = key(7, 0x1000, 0x2000, 5);
        assert!(previous.is_valid_against(Some(&previous)));
    }

    #[test]
    fn rotation_with_pinned_count_invalidates_cache() {
        let previous = key(299, 0x1000, 0x2000, 5);
        let live = key(299, 0x3000, 0x4000, 5);
        assert!(!live.is_valid_against(Some(&previous)));
    }

    #[test]
    fn growth_shifts_the_newest_message_index() {
        let previous = key(7, 0x1000, 0x2000, 5);
        let live = key(8, 0x1000, 0x2000, 5);
        assert!(!live.is_valid_against(Some(&previous)));
    }

    #[test]
    fn absent_or_unresolved_fingerprint_invalidates_cache() {
        let previous = key(7, 0x1000, 0x2000, 5);
        assert!(!previous.is_valid_against(None));
        let unresolved = ChatCacheKey::default();
        assert!(!unresolved.is_valid_against(Some(&previous)));
        assert!(!unresolved.is_valid_against(Some(&unresolved)));
    }

    #[test]
    fn content_length_participates_in_fingerprint() {
        let previous = key(7, 0x1000, 0x2000, 5);
        let live = key(7, 0x1000, 0x2000, 6);
        assert!(!live.is_valid_against(Some(&previous)));
    }

    #[test]
    fn message_walk_is_capped_above_the_osu_history_bound() {
        assert_eq!(MAX_CHAT_MESSAGES, 500);
        assert_eq!(chat_capacity(1), Some(1));
        let cap = MAX_CHAT_MESSAGES as i32;
        assert_eq!(chat_capacity(cap), Some(MAX_CHAT_MESSAGES));
        assert_eq!(chat_capacity(cap + 1), None);
        assert_eq!(chat_capacity(i32::MAX), None);
        assert_eq!(chat_capacity(0), None);
        assert_eq!(chat_capacity(-1), None);
    }
}
