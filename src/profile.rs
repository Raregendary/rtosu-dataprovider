use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerChain {
    pub module: String,
    pub base_offset: i64,
    pub offsets: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientProfile {
    pub id: String,
    pub display_name: String,
    pub process_names: Vec<String>,
    pub module_names: Vec<String>,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub pointer_width: usize,
    #[serde(default)]
    pub pointer_chains: BTreeMap<String, PointerChain>,
    #[serde(default)]
    pub patterns: BTreeMap<String, String>,
    #[serde(default)]
    pub pattern_offsets: BTreeMap<String, i64>,
}

impl ClientProfile {
    pub fn pattern(&self, key: &str) -> Result<(&str, i64)> {
        let pattern = self.patterns.get(key).map(String::as_str).ok_or_else(|| {
            anyhow::anyhow!(
                "pattern '{key}' is not present in profile '{}'; available keys: {:?}",
                self.id,
                self.patterns.keys().collect::<Vec<_>>()
            )
        })?;
        Ok((pattern, self.pattern_offsets.get(key).copied().unwrap_or(0)))
    }

    pub fn profile_for_process(process_name: &str) -> Option<&'static str> {
        let lower = process_name.to_ascii_lowercase();
        if lower.contains("tourney") || lower.contains("tournament") || lower.contains("arcade") {
            Some("tournament")
        } else if lower.contains("osu") {
            Some("stable")
        } else {
            None
        }
    }
}

pub fn available_profiles() -> Vec<&'static str> {
    vec!["stable", "tournament"]
}

pub fn load_profile(name: &str) -> Result<ClientProfile> {
    let name = name.to_ascii_lowercase();
    let source = match name.as_str() {
        "stable" => include_str!("../profiles/stable.json"),
        "tournament" | "tourney" => include_str!("../profiles/tournament.json"),
        other => bail!("unknown profile '{other}'; available profiles: stable, tournament"),
    };
    serde_json::from_str(source).with_context(|| format!("loading embedded {name} profile"))
}

pub fn load_profile_file(path: impl AsRef<Path>) -> Result<ClientProfile> {
    let path = path.as_ref();
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading profile {}", path.display()))?;
    serde_json::from_str(&source).with_context(|| format!("parsing profile {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{ClientProfile, load_profile};

    #[test]
    fn embedded_profiles_parse() {
        let stable = load_profile("stable").unwrap();
        let tournament = load_profile("tournament").unwrap();
        assert_eq!(stable.pointer_width, 4);
        assert_eq!(tournament.pointer_width, 4);
        assert!(stable.pattern("game_time_ptr").is_ok());
        assert!(tournament.pattern("tournament_chat_engine").is_ok());
    }

    #[test]
    fn classifies_client_process_names() {
        assert_eq!(
            ClientProfile::profile_for_process("osu!.exe"),
            Some("stable")
        );
        assert_eq!(
            ClientProfile::profile_for_process("osu!.Tourney.exe"),
            Some("tournament")
        );
        assert_eq!(ClientProfile::profile_for_process("notepad.exe"), None);
    }
}
