//! Launching Chrome. Runs use a headless browser controlled over a private pipe
//! (`--remote-debugging-pipe`): no debugging port exists for other local processes
//! to find. Each site gets its own profile folder, owner-only.

use crate::cdp::Cdp;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Flags shared by headless runs and visible session setup. They must match, or
/// the session's cookies can't be read by the runs.
const COMMON_FLAGS: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-sync",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-default-apps",
    "--disable-features=Translate,MediaRouter,OptimizationHints",
    // Without this, Chrome on macOS asks for Keychain access for every profile.
    // Profile cookies are then protected by the folder's permissions only (see
    // docs/THREAT_MODEL.md, A7).
    "--use-mock-keychain",
    "--password-store=basic",
];

/// The browser for runs: `PSS_BROWSER_PATH` if set, otherwise the pinned headless
/// shell (`install-browser`). Never the person's own Chrome: starting that from a
/// background process trips macOS App Management and shares its update cycle.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PSS_BROWSER_PATH") {
        return Some(PathBuf::from(p));
    }
    crate::headless::installed()
}

/// A visible browser for session setup (signing in), which the headless shell can't
/// show. Only started when the person asks for it.
pub fn find_desktop_chrome() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
        .iter()
        .map(PathBuf::from)
        .collect()
    } else if cfg!(windows) {
        ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|v| std::env::var_os(v))
            .flat_map(|base| {
                let base = PathBuf::from(base);
                [base.join("Google/Chrome/Application/chrome.exe"), base.join("Microsoft/Edge/Application/msedge.exe"), base.join("BraveSoftware/Brave-Browser/Application/brave.exe")]
            })
            .collect()
    } else {
        ["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"].iter().map(PathBuf::from).collect()
    };
    candidates.into_iter().find(|p| p.exists())
}

pub use crate::paths::private_dir;
pub use sys::{kill_process, profile_in_use};

/// On Windows, Chrome starts the native host with inheritable stdio handles. Clear
/// that so browsers this host starts (including a detached session window) don't
/// keep the native-messaging pipe open.
pub fn keep_stdio_to_ourselves() {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let _ = sys::inheritable(std::io::stdin().as_raw_handle(), false);
        let _ = sys::inheritable(std::io::stdout().as_raw_handle(), false);
        let _ = sys::inheritable(std::io::stderr().as_raw_handle(), false);
    }
}

pub struct Browser {
    child: Child,
    pub cdp: Cdp,
    /// Held for the whole run: no other run or session window may use the profile.
    _lock: fs::File,
    /// This run's disk cache, outside the profile, deleted when the run ends.
    cache: PathBuf,
}

/// Our own lock beside each profile. Chrome's SingletonLock covers visible windows
/// but not the headless shell, so runs (host and runner share profiles) take this.
fn lock_profile(profile: &Path) -> Result<fs::File, String> {
    let path = profile.with_extension("lock");
    let file = fs::File::options().create(true).truncate(false).write(true).open(&path).map_err(|e| e.to_string())?;
    file.try_lock().map_err(|_| "This site's browser profile is busy: another run is using it. Try again when it finishes.".to_string())?;
    Ok(file)
}

impl Browser {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn launch_headless(chrome: &Path, profile: &Path) -> Result<Browser, String> {
        private_dir(profile).map_err(|e| format!("Could not create the browser profile: {e}"))?;
        let lock = lock_profile(profile)?;
        if profile_in_use(profile) {
            return Err("The session window for this site is still open. Close it, then retry.".into());
        }
        // Page caches go to a folder of their own and are deleted after the run, so
        // scraped pages don't stay on disk in the clear.
        let cache = profile.with_extension(format!("cache-{}", pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<6>())));
        private_dir(&cache).map_err(|e| e.to_string())?;
        let mut command = Command::new(chrome);
        command
            .args(COMMON_FLAGS)
            .arg("--headless=new")
            .arg("--remote-debugging-pipe")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--disk-cache-dir={}", cache.display()))
            .arg("--disable-gpu-shader-disk-cache")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Last resort for containers without user namespaces. The sandbox is what
        // contains a malicious page, so this is opt-in and documented (docs/RUNNER.md).
        if std::env::var_os("PSS_BROWSER_NO_SANDBOX").is_some_and(|v| v == "1") {
            command.arg("--no-sandbox");
        }
        let (child, writer, reader) = match sys::spawn_with_pipe(command) {
            Ok(spawned) => spawned,
            Err(e) => {
                let _ = fs::remove_dir_all(&cache);
                return Err(e);
            }
        };
        Ok(Browser { child, cdp: Cdp::new(writer, reader), _lock: lock, cache })
    }

    /// Asks Chrome to close, then makes sure it is gone.
    pub fn close(mut self) {
        let _ = self.cdp.call(None, "Browser.close", serde_json::json!({}), Duration::from_secs(3));
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Never leave a headless browser behind, even after a panic.
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.cache);
    }
}

/// A visible browser on the site's profile, for signing in. It is detached: the
/// person closes it when done, and the host process may exit meanwhile.
pub fn open_visible(chrome: &Path, profile: &Path, url: &str) -> Result<(), String> {
    private_dir(profile).map_err(|e| format!("Could not create the browser profile: {e}"))?;
    // Released straight away: a visible window then holds Chrome's own lock.
    drop(lock_profile(profile).map_err(|_| "A run is using this site's browser right now. Try again when it finishes.".to_string())?);
    if profile_in_use(profile) {
        return Err("This site's session window is already open.".into());
    }
    Command::new(chrome)
        .args(COMMON_FLAGS)
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--new-window")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start Chrome: {e}"))
}

#[cfg(unix)]
mod sys {
    use std::fs::{self, File};
    use std::os::unix::io::{FromRawFd, RawFd};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command};

    /// True while a Chrome process still has the profile open. Chrome's
    /// `SingletonLock` is a symlink to `<host>-<pid>`; a lock left by a crashed or
    /// killed browser is removed so the site isn't blocked forever.
    pub fn profile_in_use(profile: &Path) -> bool {
        let lock = profile.join("SingletonLock");
        let Ok(target) = fs::read_link(&lock) else {
            return fs::symlink_metadata(&lock).is_ok();
        };
        let pid = target.to_string_lossy().rsplit('-').next().and_then(|p| p.parse::<i32>().ok());
        match pid {
            Some(pid) if pid > 0 && !process_alive(pid) => {
                for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
                    let _ = fs::remove_file(profile.join(name));
                }
                false
            }
            _ => true,
        }
    }

    fn process_alive(pid: i32) -> bool {
        #[allow(unsafe_code)]
        // SAFETY: signal 0 only checks that the process exists; nothing is sent.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Kills a process by pid (the watchdog uses it on a browser that outlives its run).
    pub fn kill_process(pid: u32) {
        #[allow(unsafe_code)]
        // SAFETY: sends SIGKILL to a pid this process spawned and has not yet reaped.
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }

    /// Chrome's `--remote-debugging-pipe` reads commands on fd 3 and writes on fd 4.
    pub fn spawn_with_pipe(mut command: Command) -> Result<(Child, File, File), String> {
        let (cmd_read, cmd_write) = pipe()?;
        let (resp_read, resp_write) = pipe()?;
        give_pipe_to_child(&mut command, cmd_read, resp_write);
        let child = command.spawn().map_err(|e| format!("Could not start Chrome: {e}"));
        // The child has its copies; close ours so EOF works.
        close(cmd_read);
        close(resp_write);
        let (writer, reader) = (file_from(cmd_write), file_from(resp_read));
        Ok((child?, writer, reader))
    }

    fn pipe() -> Result<(RawFd, RawFd), String> {
        let mut fds = [0 as RawFd; 2];
        #[allow(unsafe_code)]
        // SAFETY: `fds` is a valid two-element array for pipe(2) to fill.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(format!("Could not create a pipe: {}", std::io::Error::last_os_error()));
        }
        for fd in fds {
            #[allow(unsafe_code)]
            // SAFETY: fcntl on a descriptor we just created.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        Ok((fds[0], fds[1]))
    }

    fn close(fd: RawFd) {
        #[allow(unsafe_code)]
        // SAFETY: closing a descriptor we own and never use again.
        unsafe {
            libc::close(fd);
        }
    }

    fn file_from(fd: RawFd) -> File {
        #[allow(unsafe_code)]
        // SAFETY: `fd` is an open descriptor owned by nothing else; File takes ownership.
        unsafe {
            File::from_raw_fd(fd)
        }
    }

    fn give_pipe_to_child(command: &mut Command, read_end: RawFd, write_end: RawFd) {
        #[allow(unsafe_code)]
        // SAFETY: the closure runs in the forked child before exec and only calls fcntl
        // and dup2, which are async-signal-safe. Both ends are first moved above fd 10 so
        // that neither dup2 can clobber the other or be a no-op (dup2(3, 3) would keep
        // close-on-exec set); close-on-exec is then cleared on 3 and 4.
        unsafe {
            command.pre_exec(move || {
                let r = libc::fcntl(read_end, libc::F_DUPFD_CLOEXEC, 10);
                let w = libc::fcntl(write_end, libc::F_DUPFD_CLOEXEC, 10);
                if r < 0 || w < 0 || libc::dup2(r, 3) < 0 || libc::dup2(w, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 || libc::fcntl(4, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::{self, File};
    use std::os::windows::io::{AsRawHandle, OwnedHandle};
    use std::path::Path;
    use std::process::{Child, Command};

    /// Chrome holds `lockfile` open while it uses the profile; deleting it fails then.
    /// A lock left by a crashed browser deletes fine, which also clears it.
    pub fn profile_in_use(profile: &Path) -> bool {
        let lock = profile.join("lockfile");
        lock.exists() && fs::remove_file(&lock).is_err()
    }

    pub fn kill_process(pid: u32) {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
    }

    /// On Windows Chrome takes the pipe as inheritable handles named on its command
    /// line: `--remote-debugging-io-pipes=<read>,<write>`.
    pub fn spawn_with_pipe(mut command: Command) -> Result<(Child, File, File), String> {
        let (cmd_read, cmd_write) = std::io::pipe().map_err(|e| format!("Could not create a pipe: {e}"))?;
        let (resp_read, resp_write) = std::io::pipe().map_err(|e| format!("Could not create a pipe: {e}"))?;
        // Inheritable only for the moment of the spawn, so other children (curl,
        // taskkill) don't pick these handles up.
        inheritable(cmd_read.as_raw_handle(), true)?;
        inheritable(resp_write.as_raw_handle(), true)?;
        command.arg(format!("--remote-debugging-io-pipes={},{}", cmd_read.as_raw_handle() as usize, resp_write.as_raw_handle() as usize));
        let child = command.spawn().map_err(|e| format!("Could not start Chrome: {e}"));
        let _ = inheritable(cmd_read.as_raw_handle(), false);
        let _ = inheritable(resp_write.as_raw_handle(), false);
        let child = child?;
        // The child has its copies; close ours so EOF works.
        drop(cmd_read);
        drop(resp_write);
        Ok((child, File::from(OwnedHandle::from(cmd_write)), File::from(OwnedHandle::from(resp_read))))
    }

    /// Sets or clears a handle's inherit flag.
    pub fn inheritable(handle: std::os::windows::io::RawHandle, on: bool) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        #[allow(unsafe_code)]
        // SAFETY: `handle` is a live handle owned by this process; only its inherit
        // flag changes.
        let ok = unsafe { SetHandleInformation(handle as _, HANDLE_FLAG_INHERIT, if on { HANDLE_FLAG_INHERIT } else { 0 }) };
        if ok == 0 {
            return Err(format!("Could not share the pipe with Chrome: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}
