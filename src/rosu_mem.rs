use anyhow::{Context, Result};
use rosu_mem::process::{Process, ProcessTraits};
use rosu_mem::signature::Signature;
use std::str::FromStr;

pub fn read_bytes(process_name: &str, address: u64, length: usize) -> Result<Vec<u8>> {
    let process =
        Process::initialize(process_name, &[]).context("initializing rosu-mem process")?;
    let address = usize::try_from(address).context("address does not fit in usize")?;
    let mut bytes = vec![0u8; length];
    process
        .read(address, length, &mut bytes)
        .context("reading bytes with rosu-mem")?;
    Ok(bytes)
}

pub fn read_u32(process_name: &str, address: u64) -> Result<u32> {
    let process =
        Process::initialize(process_name, &[]).context("initializing rosu-mem process")?;
    let address = usize::try_from(address).context("address does not fit in usize")?;
    process
        .read_u32(address)
        .context("reading u32 with rosu-mem")
}

pub fn read_i32(process_name: &str, address: u64) -> Result<i32> {
    let process =
        Process::initialize(process_name, &[]).context("initializing rosu-mem process")?;
    let address = usize::try_from(address).context("address does not fit in usize")?;
    process
        .read_i32(address)
        .context("reading i32 with rosu-mem")
}

pub fn find_signature(process_name: &str, pattern: &str) -> Result<u64> {
    let signature = Signature::from_str(pattern).context("parsing rosu-mem signature")?;
    let process =
        Process::initialize(process_name, &[]).context("initializing rosu-mem process")?;
    let address: usize = process
        .read_signature(&signature)
        .context("scanning process with rosu-mem")?;
    Ok(address as u64)
}
