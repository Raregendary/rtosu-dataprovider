use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub base: u64,
    pub size: u64,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryRegion {
    pub base: u64,
    pub size: usize,
    pub state: u32,
    pub protection: u32,
    pub kind: u32,
}

impl MemoryRegion {
    pub fn is_readable(&self) -> bool {
        if self.state != 0x1000 || self.protection & 0x100 != 0 {
            return false;
        }
        matches!(
            self.protection & 0xff,
            0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80
        )
    }

    pub fn is_executable(&self) -> bool {
        self.state == 0x1000
            && self.protection & 0x100 == 0
            && matches!(self.protection & 0xff, 0x20 | 0x40 | 0x80)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{MemoryRegion, ModuleInfo, ProcessInfo};
    use crate::address::{checked_add, checked_add_signed};
    use crate::pattern::BytePattern;
    use anyhow::{Context, Result, anyhow, bail};
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
        Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQueryEx};
    use windows_sys::Win32::System::SystemInformation::{
        IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, IsWow64Process, IsWow64Process2, OpenProcess,
        PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    const MAXIMUM_USER_ADDRESS_64: usize = 0x0000_7fff_ffff_ffff;
    const SCAN_CHUNK_SIZE: usize = 1024 * 1024;

    pub struct ProcessMemory {
        handle: HANDLE,
        pid: u32,
        pointer_size: usize,
    }

    impl ProcessMemory {
        pub fn open(pid: u32) -> Result<Self> {
            Self::open_with_pointer_size(pid, None)
        }

        pub fn open_with_pointer_size(pid: u32, pointer_size: Option<usize>) -> Result<Self> {
            crate::instr_scope!(ProcessOpen);
            if pid == 0 {
                bail!("process id must be greater than zero");
            }
            if let Some(pointer_size) = pointer_size
                && pointer_size != 4
                && pointer_size != 8
            {
                bail!("pointer size must be 4 or 8 bytes");
            }

            let handle =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
            let handle = if handle.is_null() {
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) }
            } else {
                handle
            };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("opening process");
            }
            let pointer_size = pointer_size.unwrap_or_else(|| detect_pointer_size(handle));
            Ok(Self {
                handle,
                pid,
                pointer_size,
            })
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn pointer_size(&self) -> usize {
            self.pointer_size
        }

        pub fn is_alive(&self) -> bool {
            let mut exit_code = 0u32;
            let result = unsafe { GetExitCodeProcess(self.handle, &mut exit_code) };
            result != 0 && exit_code == 259
        }

        pub fn is_foreground(&self) -> bool {
            unsafe {
                let hwnd = GetForegroundWindow();
                if hwnd.is_null() {
                    return false;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                pid == self.pid
            }
        }

        pub fn read_bytes(&self, address: u64, length: usize) -> Result<Vec<u8>> {
            if length == 0 {
                return Ok(Vec::new());
            }
            crate::instr_scope!(ReadBytes);
            crate::instr_bytes!(length);
            let mut bytes = vec![0u8; length];
            let mut bytes_read = 0usize;
            #[cfg_attr(not(feature = "instr"), allow(unused_variables))]
            let started = std::time::Instant::now();
            let result = unsafe {
                ReadProcessMemory(
                    self.handle,
                    address as *const c_void,
                    bytes.as_mut_ptr() as *mut c_void,
                    length,
                    &mut bytes_read,
                )
            };
            #[cfg(feature = "instr")]
            if started.elapsed().as_millis() > 50 {
                eprintln!(
                    "[instr-trace] slow read {length}B @ 0x{address:X} took {:?} ok={}",
                    started.elapsed(),
                    result != 0
                );
            }
            if result == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("reading {length} bytes at 0x{address:X}"));
            }
            if bytes_read != length {
                bail!("read {} of {length} bytes at 0x{address:X}", bytes_read);
            }
            Ok(bytes)
        }

        pub fn read_u8(&self, address: u64) -> Result<u8> {
            Ok(self.read_bytes(address, 1)?[0])
        }

        pub fn read_u32(&self, address: u64) -> Result<u32> {
            let bytes = self.read_bytes(address, size_of::<u32>())?;
            Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }

        pub fn read_u16(&self, address: u64) -> Result<u16> {
            let bytes = self.read_bytes(address, size_of::<u16>())?;
            Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
        }

        pub fn read_i16(&self, address: u64) -> Result<i16> {
            Ok(self.read_u16(address)? as i16)
        }

        pub fn read_i64(&self, address: u64) -> Result<i64> {
            Ok(self.read_u64(address)? as i64)
        }

        pub fn read_i32(&self, address: u64) -> Result<i32> {
            Ok(self.read_u32(address)? as i32)
        }

        pub fn read_u64(&self, address: u64) -> Result<u64> {
            let bytes = self.read_bytes(address, size_of::<u64>())?;
            Ok(u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]))
        }

        pub fn read_pointer(&self, address: u64) -> Result<u64> {
            match self.pointer_size {
                4 => Ok(self.read_u32(address)? as u64),
                8 => self.read_u64(address),
                size => bail!("unsupported pointer size {size}"),
            }
        }

        pub fn read_indirect_pointer(&self, address: u64) -> Result<u64> {
            let pointer = self.read_pointer(address)?;
            if pointer == 0 {
                bail!("indirect pointer at 0x{address:X} is null");
            }
            self.read_pointer(pointer)
        }

        pub fn read_f32(&self, address: u64) -> Result<f32> {
            Ok(f32::from_bits(self.read_u32(address)?))
        }

        pub fn read_f64(&self, address: u64) -> Result<f64> {
            Ok(f64::from_bits(self.read_u64(address)?))
        }

        pub fn read_c_string(&self, address: u64, max_length: usize) -> Result<String> {
            let mut bytes = Vec::with_capacity(max_length.min(256));
            for offset in 0..max_length {
                let byte = self.read_u8(address + offset as u64)?;
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }

        pub fn read_dotnet_string(&self, address: u64, max_chars: usize) -> Result<String> {
            let length_offset = if self.pointer_size == 4 { 4 } else { 8 };
            let length_address = checked_add(address, length_offset as u64)?;
            let length = self.read_u32(length_address)? as usize;
            if length > max_chars {
                bail!("string length {length} exceeds limit {max_chars}");
            }
            let data_address = checked_add(length_address, 4)?;
            self.read_utf16(data_address, length)
        }

        pub fn read_dotnet_string_from_pointer(
            &self,
            address: u64,
            max_chars: usize,
        ) -> Result<String> {
            let string_address = self.read_pointer(address)?;
            if string_address == 0 {
                bail!("string pointer is null");
            }
            self.read_dotnet_string(string_address, max_chars)
        }

        pub fn read_utf16(&self, address: u64, length: usize) -> Result<String> {
            if length == 0 {
                return Ok(String::new());
            }
            let byte_length = length
                .checked_mul(2)
                .ok_or_else(|| anyhow!("UTF-16 length overflow"))?;
            let bytes = self.read_bytes(address, byte_length)?;
            let units = bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            Ok(String::from_utf16_lossy(&units))
        }

        pub fn command_line(&self) -> Result<String> {
            type NtQueryInformationProcessFn = unsafe extern "system" fn(
                process_handle: HANDLE,
                process_information_class: u32,
                process_information: *mut c_void,
                process_information_length: u32,
                return_length: *mut u32,
            ) -> i32;

            let ntdll = unsafe { GetModuleHandleA(b"ntdll.dll\0".as_ptr()) };
            if ntdll.is_null() {
                bail!("failed to get ntdll handle");
            }
            let nt_query_info_proc =
                unsafe { GetProcAddress(ntdll, b"NtQueryInformationProcess\0".as_ptr()) };
            let Some(nt_query_info_proc) = nt_query_info_proc else {
                bail!("failed to resolve NtQueryInformationProcess");
            };
            let nt_query: NtQueryInformationProcessFn =
                unsafe { std::mem::transmute(nt_query_info_proc) };

            // 1. Try ProcessWow64Information (26)
            let mut wow64_peb: usize = 0;
            let status = unsafe {
                nt_query(
                    self.handle,
                    26,
                    &mut wow64_peb as *mut _ as *mut c_void,
                    size_of::<usize>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if status >= 0 && wow64_peb != 0 {
                let params_ptr = self.read_u32((wow64_peb + 0x10) as u64)? as u64;
                if params_ptr != 0 {
                    let len = self.read_u16(params_ptr + 0x40)? as usize;
                    let buf_ptr = self.read_u32(params_ptr + 0x44)? as u64;
                    if buf_ptr != 0 && len > 0 {
                        let chars_len = len / 2;
                        return self.read_utf16(buf_ptr, chars_len);
                    }
                }
            }

            // 2. Try ProcessBasicInformation (0)
            #[repr(C)]
            struct ProcessBasicInformation {
                exit_status: i32,
                peb_base_address: *mut c_void,
                affinity_mask: usize,
                base_priority: i32,
                unique_process_id: usize,
                inherited_from_unique_process_id: usize,
            }
            let mut pbi: ProcessBasicInformation = unsafe { zeroed() };
            let status = unsafe {
                nt_query(
                    self.handle,
                    0,
                    &mut pbi as *mut _ as *mut c_void,
                    size_of::<ProcessBasicInformation>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if status >= 0 && !pbi.peb_base_address.is_null() {
                let peb_addr = pbi.peb_base_address as u64;
                let params_ptr = self.read_u64(peb_addr + 0x20)?;
                if params_ptr != 0 {
                    let len = self.read_u16(params_ptr + 0x70)? as usize;
                    let buf_ptr = self.read_u64(params_ptr + 0x78)?;
                    if buf_ptr != 0 && len > 0 {
                        let chars_len = len / 2;
                        return self.read_utf16(buf_ptr, chars_len);
                    }
                }
            }

            bail!("failed to query command line from process PEB");
        }

        pub fn process_image_path(&self) -> Result<String> {
            unsafe {
                let mut buffer = [0u16; 1024];
                let mut size = buffer.len() as u32;
                let res =
                    QueryFullProcessImageNameW(self.handle, 0, buffer.as_mut_ptr(), &mut size);
                if res != 0 && size > 0 {
                    Ok(String::from_utf16_lossy(&buffer[..size as usize]))
                } else {
                    bail!("failed to query process image name");
                }
            }
        }

        pub fn resolve_pointer_chain(&self, base: u64, offsets: &[u64]) -> Result<u64> {
            let mut current = base;
            for (index, offset) in offsets.iter().enumerate() {
                let pointer = self
                    .read_pointer(current)
                    .with_context(|| format!("reading pointer {} at 0x{current:X}", index + 1))?;
                current = pointer
                    .checked_add(*offset)
                    .ok_or_else(|| anyhow!("pointer chain overflow at step {}", index + 1))?;
            }
            Ok(current)
        }

        pub fn read_pointer_chain(&self, base: u64, offsets: &[u64]) -> Result<u64> {
            let address = self.resolve_pointer_chain(base, offsets)?;
            self.read_pointer(address)
        }

        pub fn resolve_pointer_chain_signed(&self, base: u64, offsets: &[i64]) -> Result<u64> {
            let mut current = base;
            for (index, offset) in offsets.iter().enumerate() {
                let pointer = self
                    .read_pointer(current)
                    .with_context(|| format!("reading pointer {} at 0x{current:X}", index + 1))?;
                current = checked_add_signed(pointer, *offset)
                    .with_context(|| format!("pointer chain overflow at step {}", index + 1))?;
            }
            Ok(current)
        }

        pub fn read_pointer_chain_signed(&self, base: u64, offsets: &[i64]) -> Result<u64> {
            let address = self.resolve_pointer_chain_signed(base, offsets)?;
            self.read_pointer(address)
        }

        pub fn query_regions(&self) -> Result<Vec<MemoryRegion>> {
            crate::instr_scope!(ScanRegions);
            let mut regions = Vec::new();
            let mut address = 0usize;
            let max_address = if usize::BITS == 64 {
                MAXIMUM_USER_ADDRESS_64
            } else {
                0x7fff_ffff
            };

            while address < max_address {
                let mut information: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
                let queried = unsafe {
                    VirtualQueryEx(
                        self.handle,
                        address as *const c_void,
                        &mut information,
                        size_of::<MEMORY_BASIC_INFORMATION>(),
                    )
                };
                if queried == 0 {
                    break;
                }
                let base = information.BaseAddress as usize;
                let size = information.RegionSize;
                if size == 0 {
                    break;
                }
                regions.push(MemoryRegion {
                    base: base as u64,
                    size,
                    state: information.State,
                    protection: information.Protect,
                    kind: information.Type,
                });
                let next = match base.checked_add(size) {
                    Some(next) => next,
                    None => break,
                };
                if next <= address {
                    break;
                }
                address = next;
            }

            Ok(regions)
        }

        pub fn scan_pattern(
            &self,
            pattern: &BytePattern,
            range: Option<(u64, u64)>,
            max_matches: usize,
            max_bytes: usize,
        ) -> Result<Vec<u64>> {
            crate::instr_scope!(ScanPattern);
            let mut regions = if let Some((base, size)) = range {
                let end = base
                    .checked_add(size)
                    .ok_or_else(|| anyhow!("scan range overflow"))?;
                self.query_regions()?
                    .into_iter()
                    .filter_map(|region| {
                        let region_end = region.base.checked_add(region.size as u64)?;
                        let clipped_start = region.base.max(base);
                        let clipped_end = region_end.min(end);
                        if clipped_start >= clipped_end {
                            return None;
                        }
                        Some(MemoryRegion {
                            base: clipped_start,
                            size: usize::try_from(clipped_end - clipped_start).ok()?,
                            state: region.state,
                            protection: region.protection,
                            kind: region.kind,
                        })
                    })
                    .collect()
            } else {
                self.query_regions()?
            };
            regions.sort_by_key(|region| if region.is_executable() { 0 } else { 1 });

            let overlap = pattern.len().saturating_sub(1);
            let mut budget = max_bytes;
            let mut matches = Vec::new();

            for region in regions {
                if !region.is_readable() || matches.len() >= max_matches || budget == 0 {
                    continue;
                }
                let region_limit = region.size.min(budget);
                let mut offset = 0usize;
                while offset < region_limit && matches.len() < max_matches {
                    let remaining = region_limit - offset;
                    let chunk_size = remaining.min(SCAN_CHUNK_SIZE);
                    if chunk_size < pattern.len() {
                        break;
                    }
                    let address = region.base + offset as u64;
                    let bytes = match self.read_bytes(address, chunk_size) {
                        Ok(bytes) => bytes,
                        Err(_) => break,
                    };
                    let found = pattern.find_all(
                        &bytes,
                        address,
                        max_matches.saturating_sub(matches.len()),
                    );
                    for found_address in found {
                        if matches.last().copied() != Some(found_address) {
                            matches.push(found_address);
                        }
                    }
                    if chunk_size <= overlap {
                        break;
                    }
                    let advance = chunk_size - overlap;
                    budget = budget.saturating_sub(advance);
                    offset += advance;
                }
            }

            Ok(matches)
        }
    }

    impl Drop for ProcessMemory {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    unsafe impl Send for ProcessMemory {}
    unsafe impl Sync for ProcessMemory {}

    fn detect_pointer_size(handle: HANDLE) -> usize {
        let mut process_machine = 0u16;
        let mut native_machine = 0u16;
        if unsafe { IsWow64Process2(handle, &mut process_machine, &mut native_machine) } != 0 {
            return match process_machine {
                IMAGE_FILE_MACHINE_I386 => 4,
                IMAGE_FILE_MACHINE_AMD64 | IMAGE_FILE_MACHINE_ARM64 => 8,
                _ => usize::BITS as usize / 8,
            };
        }
        let mut is_wow64 = 0i32;
        if unsafe { IsWow64Process(handle, &mut is_wow64) } != 0 && is_wow64 != 0 {
            4
        } else {
            usize::BITS as usize / 8
        }
    }

    #[inline]
    fn u16_ascii_lower(c: u16) -> u16 {
        if (b'A' as u16..=b'Z' as u16).contains(&c) {
            c + 32
        } else {
            c
        }
    }

    pub fn list_processes(name_filter: Option<&str>) -> Result<Vec<ProcessInfo>> {
        crate::instr_scope!(ProcessDiscovery);
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("creating process snapshot");
        }

        let filter_u16: Option<Vec<u16>> =
            name_filter.map(|f| f.encode_utf16().map(u16_ascii_lower).collect());

        let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut processes = Vec::new();
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            let matches = match &filter_u16 {
                None => true,
                Some(needle) => {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let exe_slice = &entry.szExeFile[..len];
                    if needle.is_empty() {
                        true
                    } else if needle.len() > exe_slice.len() {
                        false
                    } else {
                        exe_slice.windows(needle.len()).any(|window| {
                            window
                                .iter()
                                .zip(needle.iter())
                                .all(|(&a, &b)| u16_ascii_lower(a) == b)
                        })
                    }
                }
            };
            if matches {
                processes.push(ProcessInfo {
                    pid: entry.th32ProcessID,
                    name: utf16_to_string(&entry.szExeFile),
                });
            }
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe {
            CloseHandle(snapshot);
        }
        Ok(processes)
    }

    pub fn list_modules(pid: u32) -> Result<Vec<ModuleInfo>> {
        let flags = TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32;
        let snapshot = unsafe { CreateToolhelp32Snapshot(flags, pid) };
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("creating module snapshot for pid {pid}"));
        }

        let mut entry: MODULEENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let mut modules = Vec::new();
        let mut has_entry = unsafe { Module32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            let name = utf16_to_string(&entry.szModule);
            let path = utf16_to_string(&entry.szExePath);
            modules.push(ModuleInfo {
                name,
                base: entry.modBaseAddr as usize as u64,
                size: entry.modBaseSize as u64,
                path: (!path.is_empty()).then_some(path),
            });
            has_entry = unsafe { Module32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe {
            CloseHandle(snapshot);
        }
        Ok(modules)
    }

    fn utf16_to_string(value: &[u16]) -> String {
        let length = value
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(value.len());
        String::from_utf16_lossy(&value[..length])
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{MemoryRegion, ModuleInfo, ProcessInfo};
    use crate::pattern::BytePattern;
    use anyhow::{Result, bail};

    pub struct ProcessMemory {
        pid: u32,
    }

    impl ProcessMemory {
        pub fn open(pid: u32) -> Result<Self> {
            bail!("process memory access is only implemented on Windows (pid {pid})")
        }

        pub fn open_with_pointer_size(pid: u32, _pointer_size: Option<usize>) -> Result<Self> {
            Self::open(pid)
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn pointer_size(&self) -> usize {
            usize::BITS as usize / 8
        }

        pub fn is_alive(&self) -> bool {
            false
        }

        pub fn is_foreground(&self) -> bool {
            false
        }

        pub fn read_bytes(&self, _address: u64, _length: usize) -> Result<Vec<u8>> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_u8(&self, _address: u64) -> Result<u8> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_u32(&self, _address: u64) -> Result<u32> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_u16(&self, _address: u64) -> Result<u16> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_i16(&self, _address: u64) -> Result<i16> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_i64(&self, _address: u64) -> Result<i64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_i32(&self, _address: u64) -> Result<i32> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_u64(&self, _address: u64) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_pointer(&self, _address: u64) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_indirect_pointer(&self, _address: u64) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_f32(&self, _address: u64) -> Result<f32> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_f64(&self, _address: u64) -> Result<f64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_c_string(&self, _address: u64, _max_length: usize) -> Result<String> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_dotnet_string(&self, _address: u64, _max_chars: usize) -> Result<String> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_dotnet_string_from_pointer(
            &self,
            _address: u64,
            _max_chars: usize,
        ) -> Result<String> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_utf16(&self, _address: u64, _length: usize) -> Result<String> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn command_line(&self) -> Result<String> {
            bail!("process command line access is only implemented on Windows")
        }

        pub fn resolve_pointer_chain(&self, _base: u64, _offsets: &[u64]) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_pointer_chain(&self, _base: u64, _offsets: &[u64]) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn resolve_pointer_chain_signed(&self, _base: u64, _offsets: &[i64]) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn read_pointer_chain_signed(&self, _base: u64, _offsets: &[i64]) -> Result<u64> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn query_regions(&self) -> Result<Vec<MemoryRegion>> {
            bail!("process memory access is only implemented on Windows")
        }

        pub fn scan_pattern(
            &self,
            _pattern: &BytePattern,
            _range: Option<(u64, u64)>,
            _max_matches: usize,
            _max_bytes: usize,
        ) -> Result<Vec<u64>> {
            bail!("process memory access is only implemented on Windows")
        }
    }

    pub fn list_processes(_name_filter: Option<&str>) -> Result<Vec<ProcessInfo>> {
        bail!("process enumeration is only implemented on Windows")
    }

    pub fn list_modules(_pid: u32) -> Result<Vec<ModuleInfo>> {
        bail!("module enumeration is only implemented on Windows")
    }
}

pub use platform::{ProcessMemory, list_modules, list_processes};

pub fn module_by_name<'a>(modules: &'a [ModuleInfo], requested: &str) -> Result<&'a ModuleInfo> {
    let requested_lower = requested.to_ascii_lowercase();
    modules
        .iter()
        .find(|module| module.name.to_ascii_lowercase() == requested_lower)
        .ok_or_else(|| anyhow!("module '{requested}' was not found"))
}

pub fn module_or_main<'a>(modules: &'a [ModuleInfo], requested: &str) -> Result<&'a ModuleInfo> {
    if requested.eq_ignore_ascii_case("main") {
        return modules
            .first()
            .ok_or_else(|| anyhow!("process has no modules"));
    }
    module_by_name(modules, requested)
}
