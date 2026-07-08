#![allow(dead_code)]

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: The handle is returned by CreateToolhelp32Snapshot and owned
        // by this Snapshot wrapper, so closing it here balances acquisition.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn has_process_name_containing(needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }

    // SAFETY: CreateToolhelp32Snapshot is called for a process snapshot and
    // does not require pointers. The returned handle is wrapped and closed.
    let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(handle) => Snapshot(handle),
        Err(e) => {
            log::warn!("Process snapshot failed: {:?}", e);
            return false;
        }
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    // SAFETY: entry points to a valid PROCESSENTRY32W with dwSize initialized,
    // as required by the ToolHelp API.
    if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_err() {
        return false;
    }

    loop {
        let exe = process_entry_name(&entry).to_ascii_lowercase();
        if exe.contains(&needle) {
            return true;
        }

        // SAFETY: entry remains valid for the duration of enumeration.
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            break;
        }
    }

    false
}

fn process_entry_name(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..end])
}
