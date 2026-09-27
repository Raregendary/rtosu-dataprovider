pub mod address;
pub mod beatmap;
pub mod client;
pub mod config;
pub mod logging;
pub mod overlays;
pub mod pattern;
pub mod pp;
pub mod process;
pub mod profile;
pub mod reader;
pub mod sc;
pub mod server;
pub mod session;
pub mod tournament;
pub mod v1;
pub mod v2;
pub mod ws_filters;

#[cfg(feature = "instr")]
pub mod instr;

/// Opens an instrumentation scope. Compiles to nothing without the `instr`
/// feature, so the shipped binary is unaffected.
#[cfg(feature = "instr")]
#[macro_export]
macro_rules! instr_scope {
    ($phase:ident) => {
        let mut _instr_scope = $crate::instr::Scope::new($crate::instr::Phase::$phase);
    };
}

#[cfg(not(feature = "instr"))]
#[macro_export]
macro_rules! instr_scope {
    ($phase:ident) => {};
}

/// Attributes bytes read from the target process. Compiles to nothing without
/// the `instr` feature.
#[cfg(feature = "instr")]
#[macro_export]
macro_rules! instr_bytes {
    ($bytes:expr) => {
        $crate::instr::add_bytes($bytes as u64)
    };
}

#[cfg(not(feature = "instr"))]
#[macro_export]
macro_rules! instr_bytes {
    ($bytes:expr) => {
        let _ = $bytes;
    };
}

pub use logging::init_logging;
pub use reader::{OsuReader, OsuReaderBuilder, OsuReaderMode, OsuReaderStream};

#[cfg(feature = "rosu-mem")]
pub mod rosu_mem;

/// Helpers shared by more than one module's tests.
#[cfg(test)]
pub(crate) mod testutil {
    /// Read the key order out of a serialised JSON object without a parser that
    /// would reorder it.
    ///
    /// **`serde_json::Value` cannot answer this.** Its `Map` is a `BTreeMap`
    /// unless the `preserve_order` feature is enabled, so parsing a payload and
    /// reading the keys back returns them *sorted* -- and an order assertion
    /// written that way is satisfied by every order at once. That is the failure
    /// mode this exists to prevent, and it is not hypothetical: an assertion
    /// written against a parsed `Value` reported a real payload's top level as
    /// `client, gameplay, menu, resultsScreen, settings, tourney, userProfile`
    /// when the bytes say `client, settings, menu, ...`.
    ///
    /// Scans the bytes, so it reports what a consumer reading the response
    /// actually sees, and it reads only the object's **top level** -- nested
    /// objects need their own call on their own serialisation, which is what the
    /// per-object tests do.
    pub(crate) fn json_key_order(json: &str) -> Vec<String> {
        let bytes = json.as_bytes();
        let mut keys = Vec::new();
        let mut i = 1; // skip '{'
        while i < bytes.len() {
            if bytes[i] == b'"' {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                let raw = &json[start + 1..i];
                i += 1; // closing quote
                if i < bytes.len() && bytes[i] == b':' {
                    // The token between this colon and the next top-level comma is
                    // the value; skip it, tracking nesting.
                    let mut depth = 0i32;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => {
                                if depth == 0 {
                                    break;
                                }
                                depth -= 1;
                            }
                            b',' if depth == 0 => break,
                            b'"' => {
                                i += 1;
                                while i < bytes.len() && bytes[i] != b'"' {
                                    i += 1;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    keys.push(raw.to_string());
                }
            } else {
                i += 1;
            }
        }
        keys
    }
}
