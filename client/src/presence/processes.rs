use std::path::PathBuf;

pub(super) struct Process {
    pub name: String,
    pub exe: Option<PathBuf>,
    pub started: u64,
}

#[derive(Default)]
pub(super) struct ProcessTable {
    #[cfg(not(windows))]
    system: sysinfo::System,
}

impl ProcessTable {
    #[cfg(not(windows))]
    pub fn scan(&mut self) -> Vec<Process> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
        );
        self.system
            .processes()
            .values()
            .map(|process| Process {
                name: process.name().to_string_lossy().into_owned(),
                exe: process.exe().map(std::path::Path::to_path_buf),
                started: process.start_time(),
            })
            .collect()
    }

    #[cfg(windows)]
    pub fn scan(&mut self) -> Vec<Process> {
        windows::scan()
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use ::windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use ::windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use ::windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use ::windows::core::PWSTR;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns a successfully opened handle and closes it once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn metadata(pid: u32) -> (Option<PathBuf>, u64) {
        // sysinfo opens every process with VM_READ; presence only needs public metadata.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return (None, 0);
        };
        let handle = OwnedHandle(handle);
        let mut buffer = vec![0u16; 32_768];
        let mut size = buffer.len() as u32;
        // SAFETY: buffer is writable for size UTF-16 units and the handle stays alive.
        let exe = unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut size,
            )
        }
        .ok()
        .map(|()| PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize])));
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all four output pointers are valid and the handle stays alive.
        let started =
            unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
                .ok()
                .map(|()| {
                    let ticks = (u64::from(created.dwHighDateTime) << 32)
                        | u64::from(created.dwLowDateTime);
                    (ticks / 10_000_000).saturating_sub(11_644_473_600)
                })
                .unwrap_or(0);
        (exe, started)
    }

    pub(super) fn scan() -> Vec<Process> {
        // SAFETY: a process snapshot requests no module or process-memory access.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return Vec::new();
        };
        let snapshot = OwnedHandle(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut processes = Vec::new();
        // SAFETY: snapshot is live and entry advertises its initialized size.
        if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_err() {
            return processes;
        }
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(entry.szExeFile.len());
            let (exe, started) = metadata(entry.th32ProcessID);
            processes.push(Process {
                name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                exe,
                started,
            });
            // SAFETY: snapshot and entry remain valid until enumeration finishes.
            if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                break;
            }
        }
        processes
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn limited_query_reads_current_executable_and_start_time() {
            let (exe, started) = metadata(std::process::id());
            assert_eq!(exe.unwrap(), std::env::current_exe().unwrap());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            assert!(started > 0 && started <= now);
        }

        #[test]
        fn an_unavailable_process_has_no_metadata() {
            assert_eq!(metadata(u32::MAX), (None, 0));
        }

        #[test]
        fn snapshot_finds_current_executable_on_repeated_scans() {
            let exe = std::env::current_exe().unwrap();
            let mut table = ProcessTable::default();
            for _ in 0..3 {
                assert!(table.scan().iter().any(|p| {
                    p.exe.as_ref() == Some(&exe)
                        && p.name
                            .eq_ignore_ascii_case(exe.file_name().unwrap().to_str().unwrap())
                }));
            }
        }
    }
}
