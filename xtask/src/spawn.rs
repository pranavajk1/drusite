// SPDX-License-Identifier: AGPL-3.0-or-later

//! Starting the app under measurement, and killing it however the check ends.
//!
//! On macOS a process started from a shell inherits the shell's "responsible process" (the
//! terminal, or CI's runner), and so do the WebKit helpers its webview launches. The memory check
//! finds those helpers by their responsible process, so the app has to be started as responsible
//! for itself, which is what Terminal and LLDB do with `posix_spawn` and the "disclaim" attribute.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;

/// A running app. Dropping it kills it.
#[derive(Debug)]
pub(crate) struct App {
    inner: imp::Inner,
}

impl App {
    pub(crate) fn spawn(program: &Path, envs: &[(&str, &OsStr)]) -> Result<Self> {
        Ok(Self {
            inner: imp::spawn(program, envs)?,
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.inner.id()
    }

    /// `Some(description)` once the app has exited.
    pub(crate) fn exited(&mut self) -> Result<Option<String>> {
        self.inner.exited()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.inner.kill();
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use anyhow::{Context, Result, bail};

    #[derive(Debug)]
    pub(super) struct Inner {
        pid: libc::pid_t,
        reaped: bool,
    }

    type SetDisclaim =
        unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

    fn cstring(bytes: &[u8]) -> Result<CString> {
        CString::new(bytes).context("argument contains a NUL byte")
    }

    pub(super) fn spawn(program: &Path, envs: &[(&str, &OsStr)]) -> Result<Inner> {
        let path = cstring(program.as_os_str().as_bytes())?;
        let argv = [path.as_ptr().cast_mut(), ptr::null_mut()];

        let mut env: Vec<CString> = std::env::vars_os()
            .filter(|(k, _)| !envs.iter().any(|(e, _)| OsStr::new(e) == k))
            .map(|(k, v)| cstring(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
            .collect::<Result<_>>()?;
        for (k, v) in envs {
            env.push(cstring(&[k.as_bytes(), b"=", v.as_bytes()].concat())?);
        }
        let mut envp: Vec<*mut libc::c_char> = env.iter().map(|c| c.as_ptr().cast_mut()).collect();
        envp.push(ptr::null_mut());

        let dev_null = c"/dev/null";
        let mut pid: libc::pid_t = 0;
        // SAFETY: every pointer passed below is valid for the duration of the calls: the
        // attribute and file-action objects are initialised before use and destroyed after, and
        // argv/envp are NUL-terminated arrays of NUL-terminated strings that outlive posix_spawn.
        unsafe {
            let mut attr: libc::posix_spawnattr_t = ptr::null_mut();
            let mut actions: libc::posix_spawn_file_actions_t = ptr::null_mut();
            if libc::posix_spawnattr_init(&mut attr) != 0
                || libc::posix_spawn_file_actions_init(&mut actions) != 0
            {
                bail!("posix_spawn setup failed");
            }
            libc::posix_spawn_file_actions_addopen(
                &mut actions,
                0,
                dev_null.as_ptr(),
                libc::O_RDONLY,
                0,
            );
            let sym = libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_spawnattrs_setdisclaim".as_ptr(),
            );
            if sym.is_null() {
                eprintln!(
                    "ram: warning: cannot disclaim responsibility; webview helpers may be missed"
                );
            } else {
                let set_disclaim = std::mem::transmute::<*mut libc::c_void, SetDisclaim>(sym);
                set_disclaim(&mut attr, 1);
            }
            let rc = libc::posix_spawn(
                &mut pid,
                path.as_ptr(),
                &actions,
                &attr,
                argv.as_ptr(),
                envp.as_ptr(),
            );
            libc::posix_spawn_file_actions_destroy(&mut actions);
            libc::posix_spawnattr_destroy(&mut attr);
            if rc != 0 {
                bail!(
                    "cannot start {}: {}",
                    program.display(),
                    std::io::Error::from_raw_os_error(rc)
                );
            }
        }
        Ok(Inner { pid, reaped: false })
    }

    impl Inner {
        pub(super) fn id(&self) -> u32 {
            self.pid.unsigned_abs()
        }

        pub(super) fn exited(&mut self) -> Result<Option<String>> {
            if self.reaped {
                return Ok(Some("already exited".into()));
            }
            let mut status = 0;
            // SAFETY: waitpid on our own child with a valid status pointer.
            let rc = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            match rc {
                0 => Ok(None),
                rc if rc == self.pid => {
                    self.reaped = true;
                    Ok(Some(format!("wait status {status}")))
                }
                _ => bail!("waitpid failed: {}", std::io::Error::last_os_error()),
            }
        }

        pub(super) fn kill(&mut self) {
            if !self.reaped {
                // SAFETY: signalling and reaping our own child.
                unsafe {
                    libc::kill(self.pid, libc::SIGKILL);
                    libc::waitpid(self.pid, ptr::null_mut(), 0);
                }
                self.reaped = true;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    use anyhow::{Context, Result};

    #[derive(Debug)]
    pub(super) struct Inner(Child);

    pub(super) fn spawn(program: &Path, envs: &[(&str, &OsStr)]) -> Result<Inner> {
        let mut cmd = Command::new(program);
        cmd.stdin(Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        Ok(Inner(cmd.spawn().with_context(|| {
            format!("cannot start {}", program.display())
        })?))
    }

    impl Inner {
        pub(super) fn id(&self) -> u32 {
            self.0.id()
        }

        pub(super) fn exited(&mut self) -> Result<Option<String>> {
            Ok(self.0.try_wait()?.map(|s| s.to_string()))
        }

        pub(super) fn kill(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
