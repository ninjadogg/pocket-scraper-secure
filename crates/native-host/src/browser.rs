//! Launching Chrome. Runs use a headless browser controlled over a private pipe
//! (`--remote-debugging-pipe`): no debugging port exists for other local processes
//! to find. Each site gets its own profile folder, owner-only.

use crate::cdp::Cdp;
use std::fs::{self, File};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
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

pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PSS_BROWSER_PATH") {
        return Some(PathBuf::from(p));
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else {
        &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

pub use crate::paths::private_dir;

/// True while a Chrome process still has the profile open. Chrome's `SingletonLock`
/// is a symlink to `<host>-<pid>`; a lock left by a crashed or killed browser is
/// removed so the site isn't blocked forever.
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
    // SAFETY: sends SIGKILL to a pid this host spawned and has not yet reaped.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

pub struct Browser {
    child: Child,
    pub cdp: Cdp,
}

impl Browser {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn launch_headless(chrome: &Path, profile: &Path) -> Result<Browser, String> {
        private_dir(profile).map_err(|e| format!("Could not create the browser profile: {e}"))?;
        if profile_in_use(profile) {
            return Err("The session window for this site is still open. Close it, then retry.".into());
        }
        let (cmd_read, cmd_write) = pipe()?;
        let (resp_read, resp_write) = pipe()?;
        let mut command = Command::new(chrome);
        command
            .args(COMMON_FLAGS)
            .arg("--headless=new")
            .arg("--remote-debugging-pipe")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        give_pipe_to_child(&mut command, cmd_read, resp_write);
        let child = command.spawn().map_err(|e| format!("Could not start Chrome: {e}"))?;
        // The child has its copies; close ours so EOF works.
        close(cmd_read);
        close(resp_write);
        let writer = file_from(cmd_write);
        let reader = file_from(resp_read);
        Ok(Browser { child, cdp: Cdp::new(writer, reader) })
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
    }
}

/// A visible browser on the site's profile, for signing in. It is detached: the
/// person closes it when done, and the host process may exit meanwhile.
pub fn open_visible(chrome: &Path, profile: &Path, url: &str) -> Result<(), String> {
    private_dir(profile).map_err(|e| format!("Could not create the browser profile: {e}"))?;
    if profile_in_use(profile) {
        return Err("This site's browser is already open: a session window or a run is using it. Close the window or wait for the run, then try again.".into());
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

fn pipe() -> Result<(RawFd, RawFd), String> {
    let mut fds = [0 as RawFd; 2];
    #[allow(unsafe_code)]
    // SAFETY: `fds` is a valid two-element array for pipe(2) to fill.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(format!("Could not create a pipe: {}", std::io::Error::last_os_error()));
    }
    for fd in fds {
        set_cloexec(fd);
    }
    Ok((fds[0], fds[1]))
}

fn set_cloexec(fd: RawFd) {
    #[allow(unsafe_code)]
    // SAFETY: fcntl on a descriptor we just created.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
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

/// Chrome's `--remote-debugging-pipe` reads commands on fd 3 and writes on fd 4.
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
