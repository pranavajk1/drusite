// SPDX-License-Identifier: AGPL-3.0-or-later

//! Finding every process that belongs to the app, and how much memory each one really uses.
//!
//! "Really uses" differs per OS, and plain RSS is wrong everywhere: it counts shared libraries in
//! full in every process, so a multi-process webview looks several times its size.
//! - macOS: the physical footprint, the number Activity Monitor shows.
//! - Linux: PSS, which splits shared pages between the processes that share them.
//! - Windows: the private working set, the number Task Manager shows. (The plain working set
//!   counts shared DLLs in full in every process; with a handful of WebView2 processes that
//!   counts the same pages several times over.)
//!
//! On macOS a WKWebView's WebContent and Networking processes are children of `launchd`, not of
//! the app. They are found through the kernel's "responsible process", which is how Activity
//! Monitor groups them under the app.

use std::collections::BTreeSet;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// One process counted against the app.
#[derive(Debug)]
pub(crate) struct ProcessMemory {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) bytes: u64,
}

/// The app's root process, everything below it in the process tree, and (on macOS) every process
/// the kernel holds it responsible for.
pub(crate) fn app_processes(root: u32) -> anyhow::Result<Vec<ProcessMemory>> {
    let mut sys =
        System::new().map_err(|e| anyhow::anyhow!("cannot read the process list: {e:?}"))?;
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet),
    );

    let mut members = BTreeSet::from([root]);
    // The tree is shallow; repeat until no new descendants turn up.
    loop {
        let before = members.len();
        // On Linux, sysinfo lists every thread as a process too; only real processes count.
        for (pid, proc_) in sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind().is_none())
        {
            if let Some(parent) = proc_.parent()
                && members.contains(&parent.as_u32())
            {
                members.insert(pid.as_u32());
            }
        }
        for pid in sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind().is_none())
            .map(|(pid, _)| pid)
        {
            if responsible_pid(pid.as_u32()).is_some_and(|r| members.contains(&r)) {
                members.insert(pid.as_u32());
            }
        }
        if members.len() == before {
            break;
        }
    }

    Ok(members
        .into_iter()
        .filter_map(|pid| {
            let proc_ = sys
                .process(Pid::from_u32(pid))
                .filter(|p| p.thread_kind().is_none())?;
            Some(ProcessMemory {
                pid,
                name: proc_.name().to_string_lossy().into_owned(),
                bytes: footprint(pid).unwrap_or_else(|| proc_.memory()),
            })
        })
        .collect())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn footprint(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `info` is a valid, writable `rusage_info_v2`, the struct RUSAGE_INFO_V2 asks for.
    let rc = unsafe {
        libc::proc_pid_rusage(
            i32::try_from(pid).ok()?,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    // SAFETY: the call succeeded, so the kernel filled in the struct.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(target_os = "linux")]
fn footprint(pid: u32) -> Option<u64> {
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    let line = rollup.lines().find(|l| l.starts_with("Pss:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn footprint(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: OpenProcess takes plain values; a null handle means failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut counters = std::mem::MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX2>::zeroed();
    let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>()).ok()?;
    // SAFETY: `counters` is a zeroed, writable PROCESS_MEMORY_COUNTERS_EX2 of `size` bytes, the
    // extended layout the call fills when given that size; the handle is ours and closed after.
    let ok = unsafe {
        let ok = K32GetProcessMemoryInfo(
            handle,
            counters.as_mut_ptr().cast::<PROCESS_MEMORY_COUNTERS>(),
            size,
        );
        CloseHandle(handle);
        ok
    };
    // SAFETY: the call succeeded, so the struct is filled in.
    (ok != 0).then(|| unsafe { counters.assume_init() }.PrivateWorkingSetSize as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn footprint(_pid: u32) -> Option<u64> {
    None
}

/// The process macOS holds responsible for `pid`, when that isn't `pid` itself.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn responsible_pid(pid: u32) -> Option<u32> {
    use std::sync::OnceLock;

    type Responsible = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
    static FUNC: OnceLock<Option<Responsible>> = OnceLock::new();
    // A private but long-standing libSystem function; looked up at run time so a future macOS
    // without it degrades to the process tree instead of failing to load.
    let func = FUNC.get_or_init(|| {
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name has no other requirements.
        let sym = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_get_pid_responsible_for_pid".as_ptr(),
            )
        };
        // SAFETY: when present, the symbol has this signature (pid_t -> pid_t).
        (!sym.is_null())
            .then(|| unsafe { std::mem::transmute::<*mut libc::c_void, Responsible>(sym) })
    });
    let pid_t = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: the function takes any pid and returns -1 for one it can't resolve.
    let responsible = unsafe { (*func)?(pid_t) };
    u32::try_from(responsible).ok().filter(|&r| r != pid)
}

#[cfg(not(target_os = "macos"))]
fn responsible_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_measures_this_process() {
        let me = std::process::id();
        let procs = app_processes(me).unwrap();
        let this = procs
            .iter()
            .find(|p| p.pid == me)
            .expect("own process listed");
        assert!(this.bytes > 0, "own process measured at 0 bytes");
        assert!(
            footprint(me).is_some()
                || cfg!(not(any(target_os = "macos", target_os = "linux", windows)))
        );
    }
}
