//! Finding the game process and inspecting its 32-bit (WoW64) modules from the 64-bit launcher.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_32BIT};

/// Owned process handle, closed on drop.
pub struct ProcessHandle(pub HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Returns the pid of the first running process whose exe name matches `exe_name` (case-insensitive).
pub fn find_process(exe_name: &str) -> Option<u32> {
    find_processes(exe_name).into_iter().next()
}

/// Pids of every running process whose exe name matches `exe_name` (case-insensitive).
pub fn find_processes(exe_name: &str) -> Vec<u32> {
    let mut found = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return found };
        let snapshot = ProcessHandle(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snapshot.0, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(exe_name) {
                found.push(entry.th32ProcessID);
            }
            ok = Process32NextW(snapshot.0, &mut entry).is_ok();
        }
    }
    found
}

/// A module loaded in the target, as seen through the 32-bit module list.
pub struct RemoteModule {
    pub name: String,
    /// Base address inside the 32-bit address space.
    pub base: u32,
}

/// Lists the 32-bit modules of a WoW64 process. Fails while the process is still
/// initialising (the loader data isn't there yet), so callers retry.
pub fn modules_32(process: HANDLE) -> windows::core::Result<Vec<RemoteModule>> {
    unsafe {
        let mut handles = vec![HMODULE::default(); 512];
        let mut needed = 0u32;
        loop {
            EnumProcessModulesEx(
                process,
                handles.as_mut_ptr(),
                (handles.len() * size_of::<HMODULE>()) as u32,
                &mut needed,
                LIST_MODULES_32BIT,
            )?;
            let count = needed as usize / size_of::<HMODULE>();
            if count <= handles.len() {
                handles.truncate(count);
                break;
            }
            handles.resize(count, HMODULE::default());
        }

        let mut modules = Vec::with_capacity(handles.len());
        for module in handles {
            let mut buf = [0u16; 260];
            let len = GetModuleBaseNameW(process, Some(module), &mut buf) as usize;
            if len == 0 {
                continue;
            }
            modules.push(RemoteModule {
                name: String::from_utf16_lossy(&buf[..len]),
                base: module.0 as usize as u32,
            });
        }
        Ok(modules)
    }
}

pub fn find_module(modules: &[RemoteModule], name: &str) -> Option<u32> {
    modules.iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| m.base)
}

/// Full path of a running process's exe.
pub fn image_path(pid: u32) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    unsafe {
        let process = ProcessHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]).into())
    }
}

/// Pid of the process that started `pid`.
pub fn parent_of(pid: u32) -> Option<u32> {
    unsafe {
        let snapshot = ProcessHandle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snapshot.0, &mut entry).is_ok();
        while ok {
            if entry.th32ProcessID == pid {
                return Some(entry.th32ParentProcessID);
            }
            ok = Process32NextW(snapshot.0, &mut entry).is_ok();
        }
        None
    }
}

/// Starts `exe` as if `parent_pid` had started it (PROC_THREAD_ATTRIBUTE_PARENT_PROCESS).
pub fn spawn_with_parent(
    exe: &std::path::Path,
    args: &str,
    dir: &std::path::Path,
    parent_pid: u32,
) -> Result<u32, String> {
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROCESS_CREATE_PROCESS,
        PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute,
    };
    use windows::core::{HSTRING, PWSTR};
    unsafe {
        let parent = ProcessHandle(
            OpenProcess(PROCESS_CREATE_PROCESS, false, parent_pid).map_err(|e| format!("Could not open the launcher process: {e}"))?,
        );
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut storage = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr() as _);
        InitializeProcThreadAttributeList(Some(list), 1, None, &mut size).map_err(|e| e.to_string())?;
        let parent_handle = parent.0;
        let result = (|| {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
                Some(&parent_handle as *const HANDLE as _),
                size_of::<HANDLE>(),
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = list;
            let mut command: Vec<u16> = format!("\"{}\" {args}", exe.display()).encode_utf16().chain([0]).collect();
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessW(
                &HSTRING::from(exe),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                &HSTRING::from(dir),
                &startup.StartupInfo,
                &mut info,
            )
            .map_err(|e| format!("Could not start {}: {e}", exe.display()))?;
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(info.hProcess);
            Ok(info.dwProcessId)
        })();
        DeleteProcThreadAttributeList(list);
        result
    }
}
