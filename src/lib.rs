pub mod address;
pub mod beatmap;
pub mod client;
pub mod config;
pub mod logging;
pub mod pattern;
pub mod pp;
pub mod process;
pub mod profile;
pub mod reader;
pub mod server;
pub mod session;
pub mod tournament;
pub mod v2;

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
