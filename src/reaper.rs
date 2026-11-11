//! Stops child processes, such as llama-server, however ForgeFlow ends, including when it is
//! killed outright or crashes, where no destructor or signal handler can run.
//!
//! For each child, ForgeFlow starts a small watcher (itself, run as `forgeflow __reap <pid>`)
//! whose stdin is a pipe that only ForgeFlow holds open. The operating system closes that
//! pipe whenever ForgeFlow exits, for any reason; the watcher then stops the child. When
//! ForgeFlow stops the child itself, it first tells the watcher so, and the watcher just exits.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// The argument that runs ForgeFlow as a watcher.
pub const REAP_ARG: &str = "__reap";
/// What ForgeFlow writes when it has stopped the child itself.
const RELEASED: u8 = b'x';
/// How long a child has to exit after SIGTERM before it is killed.
const GRACE: Duration = Duration::from_secs(5);

/// Keeps a watcher on a child process. Dropping it means the owner has stopped the child.
#[derive(Debug)]
pub struct Reaper {
    pipe: Option<ChildStdin>,
    watcher: Child,
}

/// Starts a watcher that stops `pid` if this process ends without releasing it. Returns
/// `None` if the watcher cannot start; the child then relies on normal cleanup alone.
pub fn watch(pid: u32) -> Option<Reaper> {
    let exe = std::env::current_exe().ok()?;
    let mut watcher = Command::new(exe)
        .args([REAP_ARG, &pid.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pipe = watcher.stdin.take();
    Some(Reaper { pipe, watcher })
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(mut pipe) = self.pipe.take() {
            let _ = pipe.write_all(&[RELEASED]);
        }
        let _ = self.watcher.wait();
    }
}

/// The watcher: waits for its owner to release `pid` or to end, and in the second case
/// stops `pid`, first with SIGTERM and then, if it lingers, SIGKILL.
pub fn run_watcher(pid: &str) -> ! {
    let Ok(pid) = pid.parse::<u32>() else { std::process::exit(2) };
    let mut byte = [0u8; 1];
    let released = matches!(std::io::stdin().read(&mut byte), Ok(1) if byte[0] == RELEASED);
    if !released {
        stop(pid);
    }
    std::process::exit(0)
}

fn stop(pid: u32) {
    let signal = |name: &str| Command::new("kill").args([name, &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !signal("-TERM") {
        return;
    }
    let started = Instant::now();
    while started.elapsed() < GRACE {
        // `kill -0` only checks that the process still exists.
        if !signal("-0") {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    signal("-KILL");
}
