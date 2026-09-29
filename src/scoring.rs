//! Per-mod score multipliers, the `[scoring]` section of `config.toml`.
//!
//! A tournament often wants a play rated by the mods it was set on: an EZ score
//! is not comparable to a HR score, and the in-game number cannot be rescaled
//! after the fact, because nothing else in the payload says which mod
//! contributed what. So a mod set is folded into one factor here, and the
//! reader multiplies the score it reports by it.
//!
//! **What is weighted, and what is not.** `play.score`, `resultsScreen.score`
//! and `tourney.totalScore.left/right` (the sum of the per-client weighted
//! scores). Accuracy, rank, pp, `tourney.clients[].user.*` and
//! `leaderboard[].score` are untouched -- weighting accuracy would change which
//! grade a play earns, and pp already treats mods itself.
//!
//! **Factors multiply.** `{ "HD" = 1.05, "HR" = 1.1 }` reports x1.155 for an
//! HDHR play, which is the reading of "multiplier" that composes.
//!
//! **A slot is one key.** Picking Nightcore sets the DoubleTime bit as well as
//! the Nightcore bit -- which is why the reported acronym string collapses
//! `DTNC` to `NC` -- so counting both would square the factor. A key may name
//! the slot as a group (`"DT/NC"`, which is how the shipped default table
//! writes the three such slots) or as either acronym alone; both resolve to the
//! same slot, and a slot can only carry one factor.

use crate::client::mod_bits;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One osu! mod slot: the acronym that names it in `[scoring] mod_multipliers`
/// (or the slash-separated group for a slot the game reports as more than one
/// acronym), and the bits that mean the slot is in play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModSlot {
    pub key: &'static str,
    pub bits: u32,
}

const fn slot(key: &'static str, bits: u32) -> ModSlot {
    ModSlot { key, bits }
}

/// The acronym for a play with no mods at all. It has no bit -- a modless
/// play's `play.mods.number` is `0` -- so it is a table row of its own rather
/// than a [`MOD_SLOTS`] entry.
pub const NO_MOD_KEY: &str = "NM";

/// Every slot a play's mods can occupy, in the order
/// [`crate::client::format_mods`] reports them.
///
/// This is the one table the whole feature reads: it names the keys the config
/// may carry (so an unknown acronym is rejected at load), the bits each key
/// covers (so the factor is applied once per slot, never once per bit), and the
/// rows of the shipped default table.
///
/// The three grouped rows are the slots osu! occupies with two bits at once:
/// Nightcore implies DoubleTime, Perfect implies SuddenDeath, and Cinema
/// implies Auto. Each is collapsed to a single acronym before reporting, and
/// each must therefore be a single factor.
pub const MOD_SLOTS: &[ModSlot] = &[
    slot("NF", mod_bits::NF),
    slot("EZ", mod_bits::EZ),
    slot("TD", mod_bits::TD),
    slot("HD", mod_bits::HD),
    slot("HR", mod_bits::HR),
    slot("SD/PF", mod_bits::SD | mod_bits::PF),
    slot("DT/NC", mod_bits::DT | mod_bits::NC),
    slot("RX", mod_bits::RX),
    slot("HT", mod_bits::HT),
    slot("FL", mod_bits::FL),
    slot("AT/CN", mod_bits::AT | mod_bits::CN),
    slot("SO", mod_bits::SO),
    slot("AP", mod_bits::AP),
    slot("4K", mod_bits::K4),
    slot("5K", mod_bits::K5),
    slot("6K", mod_bits::K6),
    slot("7K", mod_bits::K7),
    slot("8K", mod_bits::K8),
    slot("FI", mod_bits::FI),
    slot("RD", mod_bits::RD),
    slot("TG", mod_bits::TG),
    slot("9K", mod_bits::K9),
    slot("10K", mod_bits::K10),
    slot("1K", mod_bits::K1),
    slot("3K", mod_bits::K3),
    slot("2K", mod_bits::K2),
    slot("v2", mod_bits::SCORE_V2),
    slot("MR", mod_bits::MR),
];

/// Rows of a factor table: one per slot, plus the modless play.
const TABLE_LEN: usize = MOD_SLOTS.len() + 1;

/// Index of the modless-play row, which sits after every slot.
const NO_MOD_INDEX: usize = MOD_SLOTS.len();

/// The smallest factor a config may hold.
pub const MIN_FACTOR: f64 = 0.01;

/// The largest factor a config may hold.
///
/// A per-mod weight is a ratio a tournament decided on, not a game mechanic, so
/// the ceiling exists to keep a typo (`"EZ" = 180` for `1.8`) from overflowing
/// the `i32` score field on the first play. Combined factors multiply, so this
/// is not a bound on the reported score -- that is clamped, and warned about,
/// at the point of use.
pub const MAX_FACTOR: f64 = 100.0;

/// The shipped default table: every slot the game can report, at 1.0.
///
/// Listing all of them rather than shipping `{}` is deliberate. A config file
/// is documentation, and `mod_multipliers = { "NM" = 1.0, ... }` shows an
/// operator both that the table exists and exactly which keys it accepts, which
/// `{}` cannot. Every value being 1.0 means the feature changes nothing until
/// one of them is edited, which is what makes the table safe to ship.
pub fn default_multipliers() -> HashMap<String, f64> {
    let mut table = HashMap::with_capacity(TABLE_LEN);
    table.insert(NO_MOD_KEY.to_string(), 1.0);
    for slot in MOD_SLOTS {
        table.insert(slot.key.to_string(), 1.0);
    }
    table
}

/// Every key the config may carry, for an error message.
fn expected_keys() -> String {
    let mut keys = Vec::with_capacity(TABLE_LEN);
    keys.push(NO_MOD_KEY);
    keys.extend(MOD_SLOTS.iter().map(|slot| slot.key));
    keys.join(", ")
}

/// Resolve one `mod_multipliers` key to the slot row it sets.
///
/// Accepts a single acronym (`"NC"`) or the group form (`"DT/NC"`), case
/// insensitively and with surrounding spaces ignored, because the keys are
/// written by hand. A group may only name **one** slot: `"HD/HR"` is rejected
/// rather than silently becoming a way to write two settings at once, so the
/// group form always means "this is one osu! slot" and never anything else.
fn slot_index(key: &str) -> Result<usize> {
    let key = key.trim();
    if key.eq_ignore_ascii_case(NO_MOD_KEY) {
        return Ok(NO_MOD_INDEX);
    }

    let mut found: Option<usize> = None;
    for part in key.split('/') {
        let acronym = part.trim();
        if acronym.is_empty() {
            bail!(
                "scoring.mod_multipliers: '{key}' has an empty acronym; group keys are slash-separated (\"DT/NC\")"
            );
        }
        if acronym.eq_ignore_ascii_case(NO_MOD_KEY) {
            bail!(
                "scoring.mod_multipliers: '{key}' mixes '{NO_MOD_KEY}' with another acronym; the modless play is a key of its own"
            );
        }
        let Some(index) = MOD_SLOTS.iter().position(|slot| {
            slot.key
                .split('/')
                .any(|candidate| candidate.eq_ignore_ascii_case(acronym))
        }) else {
            bail!(
                "scoring.mod_multipliers: unknown mod acronym '{acronym}' in '{key}' (expected one of: {})",
                expected_keys()
            );
        };
        match found {
            None => found = Some(index),
            Some(previous) if previous == index => {}
            Some(_) => bail!(
                "scoring.mod_multipliers: '{key}' mixes mod slots; group only acronyms osu! reports for one slot (\"DT/NC\")"
            ),
        }
    }

    Ok(found.expect("a key always has at least one part"))
}

/// The slot name for a factor row, for error messages.
fn row_name(index: usize) -> &'static str {
    if index == NO_MOD_INDEX {
        NO_MOD_KEY
    } else {
        MOD_SLOTS[index].key
    }
}

/// A parsed `[scoring] mod_multipliers` table: one factor per osu! mod slot.
///
/// Construction is where every rule lives -- known acronyms, one factor per
/// slot, factors in range -- so the per-tick path is a multiply that cannot
/// fail. It is a fixed array, so holding one per session is cheap, and nothing
/// mutates it after startup.
#[derive(Debug, Clone, PartialEq)]
pub struct ModMultipliers {
    /// One factor per [`MOD_SLOTS`] row, plus the modless-play row last.
    factors: [f64; TABLE_LEN],
    /// How many rows the config set explicitly. Zero means "no table", which is
    /// what a disabled feature resolves to.
    entries: usize,
    /// Every factor is 1.0, so [`Self::factor_for`] is 1.0 for every mod set.
    ///
    /// Cached rather than recomputed because the reader asks on every client of
    /// every tournament tick, and the answer decides both the multiply and
    /// whether `tourney.totalScore` is recomputed at all.
    identity: bool,
}

impl Default for ModMultipliers {
    fn default() -> Self {
        Self::identity()
    }
}

impl ModMultipliers {
    /// A table that weights nothing: the replacement for the feature toggle.
    ///
    /// The toggle is resolved here rather than carried alongside it, so every
    /// consumer asks one question -- "is this the identity table?" -- instead
    /// of two, and cannot answer them inconsistently.
    pub fn identity() -> Self {
        Self {
            factors: [1.0; TABLE_LEN],
            entries: 0,
            identity: true,
        }
    }

    /// Parse and validate a `mod_multipliers` table from a config file.
    ///
    /// Errors are phrased for the file, naming the key path, because these are
    /// the messages `config validate` prints and what
    /// [`crate::config::AppConfig::validate`] returns.
    pub fn new(raw: &HashMap<String, f64>) -> Result<Self> {
        let mut multipliers = Self::identity();
        // Which config key set each row, so "the same slot twice" can name both
        // keys. HashMap iteration order is not stable, so the message may name
        // either one first; the outcome does not depend on the order.
        let mut set_by: Vec<Option<&str>> = vec![None; TABLE_LEN];

        for (key, &factor) in raw {
            if !factor.is_finite() || factor < MIN_FACTOR || factor > MAX_FACTOR {
                bail!(
                    "scoring.mod_multipliers: '{key}' = {factor} is outside the allowed range {MIN_FACTOR}..={MAX_FACTOR}"
                );
            }
            let index = slot_index(key)?;
            if let Some(previous) = set_by[index]
                && multipliers.factors[index] != factor
            {
                bail!(
                    "scoring.mod_multipliers: '{key}' and '{previous}' both set the {} slot, to {factor} and {}; keep one key per slot",
                    row_name(index),
                    multipliers.factors[index]
                );
            }
            multipliers.factors[index] = factor;
            set_by[index] = Some(key);
            multipliers.entries += 1;
        }

        multipliers.identity = multipliers.factors.iter().all(|factor| *factor == 1.0);
        Ok(multipliers)
    }

    /// A config that carries no entry at all. Distinct from
    /// [`Self::is_identity`]: a table of nothing but 1.0s is not empty, it is
    /// the shipped default.
    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// Whether this table leaves every score exactly as osu! reported it.
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    /// The product of the factors of every slot `mods_bits` occupies.
    ///
    /// A slot is counted once however many of its bits are set, which is the
    /// point of [`MOD_SLOTS`]: `DT|NC` is Nightcore, not DoubleTime times
    /// Nightcore. A modless play uses the `NM` row.
    pub fn factor_for(&self, mods_bits: u32) -> f64 {
        if self.identity {
            return 1.0;
        }
        if mods_bits == 0 {
            return self.factors[NO_MOD_INDEX];
        }
        let mut factor = 1.0;
        for (index, slot) in MOD_SLOTS.iter().enumerate() {
            if mods_bits & slot.bits != 0 {
                factor *= self.factors[index];
            }
        }
        factor
    }

    /// [`Self::factor_for`] applied to a score, as the payload reports it.
    ///
    /// `0` stays `0` for any factor, and a factor of exactly 1.0 returns the
    /// score untouched, so a table that weights nothing cannot perturb a value
    /// by round-tripping it through `f64`. Otherwise the product is rounded,
    /// then clamped to the `i32` field: 0.999 x 100_000_000 reports 99_900_000,
    /// and a factor large enough to pass `i32::MAX` reports `i32::MAX` and says
    /// so once in the log.
    pub fn apply(&self, mods_bits: u32, score: i32) -> i32 {
        if self.identity || score == 0 {
            return score;
        }
        let factor = self.factor_for(mods_bits);
        if factor == 1.0 {
            return score;
        }
        let weighted = f64::from(score) * factor;
        if weighted >= f64::from(i32::MAX) {
            note_saturation(mods_bits, score, factor);
            return i32::MAX;
        }
        weighted.round() as i32
    }
}

/// How often a saturated score may be repeated in the log, per mod set.
///
/// The clamp is a config error worth knowing about, but the packet is served
/// every tick, so warning on each one would print 60 lines a second for as long
/// as the play lasts.
const SATURATION_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Report a factor that pushed a score past `i32::MAX`, at most once per mod
/// set per minute.
fn note_saturation(mods_bits: u32, score: i32, factor: f64) {
    static LAST: Mutex<Option<(u32, Instant)>> = Mutex::new(None);
    let Ok(mut last) = LAST.lock() else {
        // A poisoned lock means an earlier warning panicked. The clamp still
        // happened; dropping the message beats panicking the poll loop over it.
        return;
    };
    let now = Instant::now();
    if let Some((warned_mods, at)) = *last
        && warned_mods == mods_bits
        && now.duration_since(at) < SATURATION_WARN_INTERVAL
    {
        return;
    }
    *last = Some((mods_bits, now));
    tracing::warn!(
        "scoring.mod_multipliers: {} x{factor} pushes a score of {score} past the i32 score field; reporting {}",
        crate::client::format_mods(mods_bits),
        i32::MAX
    );
}

#[cfg(test)]
mod tests {
    use super::{MAX_FACTOR, MIN_FACTOR, MOD_SLOTS, ModMultipliers, default_multipliers};
    use crate::client::mod_bits;

    /// Parse a table the way the config loader does, panicking on a rejection
    /// so a test that meant "this is valid" says which rule it broke.
    fn table(entries: &[(&str, f64)]) -> ModMultipliers {
        let raw = entries
            .iter()
            .map(|(key, factor)| (key.to_string(), *factor))
            .collect();
        ModMultipliers::new(&raw).unwrap_or_else(|error| panic!("{error}"))
    }

    /// The error a table is rejected with, so the message itself is pinned.
    fn rejected(entries: &[(&str, f64)]) -> String {
        let raw = entries
            .iter()
            .map(|(key, factor)| (key.to_string(), *factor))
            .collect();
        ModMultipliers::new(&raw)
            .expect_err("the table should have been rejected")
            .to_string()
    }

    /// The slot table is the contract with [`crate::client::format_mods`]:
    /// every key must be an acronym that function reports for the slot's bits,
    /// and for a grouped row the reported acronym is the last in the group.
    ///
    /// A key no play can ever match is a setting that silently does nothing,
    /// and a group whose tail is not the collapsed acronym would put the factor
    /// on the wrong side of `DTNC`/`SDPF`/`ATCN`.
    #[test]
    fn every_slot_key_is_an_acronym_the_formatter_reports() {
        for slot in MOD_SLOTS {
            let acronyms: Vec<&str> = slot.key.split('/').collect();
            let reported = crate::client::format_mods(slot.bits);
            assert_eq!(
                reported,
                *acronyms
                    .last()
                    .expect("a slot key names at least one acronym"),
                "slot '{}' covers bits {:#x}, which report as '{reported}'",
                slot.key,
                slot.bits
            );
        }
    }

    /// Every acronym the formatter can emit is accepted, and no others: a
    /// single bit reports its own acronym, so sweeping all 31 of them proves
    /// the key set is neither short nor padded.
    #[test]
    fn the_slot_table_covers_every_mod_the_game_reports() {
        let mut acronyms: Vec<&str> = Vec::new();
        for slot in MOD_SLOTS {
            acronyms.extend(slot.key.split('/'));
        }
        assert_eq!(
            acronyms.len(),
            31,
            "the formatter's table has 31 acronyms; got {acronyms:?}"
        );
        for bit in 0..31u32 {
            let reported = crate::client::format_mods(1 << bit);
            assert!(
                acronyms.contains(&reported.as_str()),
                "format_mods({:#x}) reports '{reported}', which no slot key accepts",
                1 << bit
            );
        }
    }

    /// The same osu! slot, written three ways, is one setting.
    ///
    /// `NC` is what the report says, `DT` is what a consumer may have written
    /// from memory, and `DT/NC` is how the shipped default table spells it. All
    /// three must land on the same row, or `{ "DT" = 1.5 }` would silently skip
    /// every Nightcore play while `{ "DT/NC" = 1.5 }` applied twice.
    #[test]
    fn a_grouped_key_and_either_acronym_name_the_same_slot() {
        let double_time = table(&[("DT", 1.5)]);
        let nightcore = table(&[("NC", 1.5)]);
        let grouped = table(&[("DT/NC", 1.5)]);
        let loose = table(&[(" dt / nc ", 1.5)]);

        for mods in [mod_bits::DT, mod_bits::NC, mod_bits::DT | mod_bits::NC] {
            assert_eq!(double_time.factor_for(mods), 1.5, "DT-only row, {mods:#x}");
            assert_eq!(nightcore.factor_for(mods), 1.5, "NC-only row, {mods:#x}");
            assert_eq!(grouped.factor_for(mods), 1.5, "grouped row, {mods:#x}");
            assert_eq!(loose.factor_for(mods), 1.5, "unstripped row, {mods:#x}");
        }
        assert_eq!(double_time, grouped);
        assert_eq!(nightcore, grouped);
    }

    /// Nightcore must not be counted twice: that is the whole reason the slot
    /// table exists instead of a per-bit loop.
    #[test]
    fn nightcore_is_not_double_time_times_nightcore() {
        let multipliers = table(&[("DT/NC", 1.1)]);

        assert_eq!(
            multipliers.apply(mod_bits::DT | mod_bits::NC, 1_000_000),
            1_100_000,
            "one slot, one factor"
        );
        assert_eq!(
            multipliers.apply(mod_bits::NC, 1_000_000),
            1_100_000,
            "the real Nightcore play carries both bits"
        );
        assert_eq!(
            multipliers.apply(mod_bits::DT, 1_000_000),
            1_100_000,
            "a DoubleTime-only play is the same slot"
        );
    }

    /// Different slots multiply, which is the reading of "multiplier" that
    /// composes.
    #[test]
    fn factors_of_different_slots_multiply() {
        let multipliers = table(&[("HD", 1.05), ("HR", 1.1)]);

        assert_eq!(
            multipliers.apply(mod_bits::HD | mod_bits::HR, 1_000_000),
            1_155_000
        );
        assert_eq!(multipliers.apply(mod_bits::HD, 1_000_000), 1_050_000);
        assert_eq!(
            multipliers.apply(mod_bits::NF, 1_000_000),
            1_000_000,
            "a mod with no factor is 1.0"
        );
    }

    /// `NM` is the modless play, and it applies to nothing else.
    #[test]
    fn the_no_mod_key_covers_only_a_modless_play() {
        let multipliers = table(&[("NM", 0.5)]);

        assert_eq!(multipliers.apply(0, 1_000_000), 500_000);
        assert_eq!(
            multipliers.apply(mod_bits::HD, 1_000_000),
            1_000_000,
            "a modded play does not also pay the NM factor"
        );
    }

    /// A score of 0 is 0 for every factor, and a table that weights nothing
    /// cannot perturb a value by re-rounding it through `f64`.
    #[test]
    fn zero_and_identity_factors_leave_the_score_alone() {
        assert_eq!(table(&[("EZ", 1.8)]).apply(mod_bits::EZ, 0), 0);
        assert_eq!(table(&[("NM", 0.5)]).apply(0, 0), 0);

        let one = table(&[("EZ", 1.0), ("NM", 1.0)]);
        assert!(one.is_identity(), "all-1.0 is the identity table");
        assert!(!one.is_empty(), "but the operator did fill it in");
        for score in [0, 1, 1_234_567, i32::MAX] {
            assert_eq!(one.apply(mod_bits::EZ, score), score);
            assert_eq!(one.factor_for(mod_bits::EZ), 1.0);
        }

        let empty = ModMultipliers::identity();
        assert!(empty.is_empty() && empty.is_identity());
        assert_eq!(empty.apply(mod_bits::HD | mod_bits::HR, 987_654), 987_654);
    }

    /// The weighted score is rounded to the nearest point, and a factor that
    /// would overflow the field reports the ceiling instead of wrapping.
    #[test]
    fn the_weighted_score_is_rounded_and_clamped() {
        assert_eq!(
            table(&[("EZ", 0.999)]).apply(mod_bits::EZ, 100_000_000),
            99_900_000
        );
        assert_eq!(
            table(&[("EZ", 1.000_001)]).apply(mod_bits::EZ, 1_000_000),
            1_000_001
        );
        assert_eq!(
            table(&[("EZ", 100.0)]).apply(mod_bits::EZ, i32::MAX),
            i32::MAX,
            "the field saturates rather than wrapping"
        );
    }

    /// Bad keys and bad values are rejected at parse time, with a message that
    /// names the key path and the offending key.
    #[test]
    fn unknown_and_out_of_range_entries_are_rejected() {
        let unknown = rejected(&[("DTNC", 2.0)]);
        assert!(
            unknown.contains("unknown mod acronym 'DTNC'")
                && unknown.contains("scoring.mod_multipliers"),
            "got: {unknown}"
        );

        let mixed = rejected(&[("HD/HR", 2.0)]);
        assert!(mixed.contains("mixes mod slots"), "got: {mixed}");

        let no_mod_mixed = rejected(&[("NM/NF", 2.0)]);
        assert!(no_mod_mixed.contains("NM"), "got: {no_mod_mixed}");

        for factor in [0.0, -1.0, 0.001, 100.001, f64::NAN, f64::INFINITY] {
            let message = rejected(&[("EZ", factor)]);
            assert!(
                message.contains("outside the allowed range"),
                "{factor} should be rejected, got: {message}"
            );
        }

        // The edges are inside the range, not outside it.
        for factor in [MIN_FACTOR, 1.0, MAX_FACTOR] {
            assert_eq!(table(&[("EZ", factor)]).factor_for(mod_bits::EZ), factor);
        }
    }

    /// One slot, one factor: the two spellings of it may agree, but they may
    /// not disagree, or which one wins would depend on hash order.
    #[test]
    fn one_slot_cannot_carry_two_factors() {
        let conflict = rejected(&[("DT", 1.1), ("NC", 1.2)]);
        assert!(
            conflict.contains("keep one key per slot"),
            "got: {conflict}"
        );
        assert!(
            conflict.contains("DT") && conflict.contains("NC"),
            "the message names both keys: {conflict}"
        );

        // Two spellings of one slot, agreeing, are the same table as far as any
        // consumer can tell -- including `is_empty`, which `entries` tracks and
        // which both of these answer `false` for.
        let both = table(&[("DT", 1.1), ("NC", 1.1)]);
        let grouped = table(&[("DT/NC", 1.1)]);
        for mods in [0, mod_bits::DT, mod_bits::NC, mod_bits::DT | mod_bits::NC] {
            assert_eq!(both.factor_for(mods), grouped.factor_for(mods), "{mods:#x}");
        }
        assert_eq!(both.factor_for(mod_bits::DT), 1.1);
    }

    /// The shipped default is every slot at 1.0, so a fresh config file both
    /// documents the keys and changes nothing.
    #[test]
    fn the_default_table_lists_every_slot_at_one() {
        let default = default_multipliers();
        assert_eq!(
            default.len(),
            MOD_SLOTS.len() + 1,
            "one key per slot plus NM"
        );
        assert!(
            default.contains_key("NM")
                && default.contains_key("DT/NC")
                && default.contains_key("v2")
        );
        assert!(default.values().all(|factor| *factor == 1.0));

        let multipliers =
            ModMultipliers::new(&default).expect("the default table must be a valid table");
        assert!(multipliers.is_identity() && !multipliers.is_empty());

        let every_mod = MOD_SLOTS.iter().fold(0u32, |bits, slot| bits | slot.bits);
        assert_eq!(
            multipliers.apply(every_mod, 12_345_678),
            12_345_678,
            "every mod at once, unchanged"
        );
        assert_eq!(multipliers.apply(0, 12_345_678), 12_345_678);
    }
}
