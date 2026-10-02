//! Test-only Git wrapper. Fault injection never ships in the production CLI.
use std::{
    env, fs,
    io::{self, Write},
    process::{Command, Stdio},
};
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let real = env::var("LANE_TEST_REAL_GIT").unwrap();
    let mode = env::var("LANE_TEST_MODE").unwrap_or_default();
    let target = env::var("LANE_TEST_TARGET").unwrap_or_default();
    if mode == "fail-status" && args.iter().any(|s| s == "status") && args.get(1) == Some(&target) {
        eprintln!("injected Git status failure");
        std::process::exit(128);
    }
    if mode == "interrupt" && args.iter().any(|s| s == "merge-tree") {
        #[cfg(windows)]
        unsafe {
            FreeConsole();
            let attached = AttachConsole(u32::MAX);
            if attached == 0 {
                eprintln!(
                    "cannot attach to Lane console: {}",
                    std::io::Error::last_os_error()
                );
            }
            SetConsoleCtrlHandler(Some(ignore_event), 1);
            GenerateConsoleCtrlEvent(1, 0);
        }
        #[cfg(unix)]
        unsafe {
            kill(getppid(), 2);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::process::exit(130);
    }
    let output = Command::new(&real)
        .args(&args)
        .stdin(Stdio::inherit())
        .output()
        .unwrap();
    let trigger = if mode == "worker-after-preflight" || mode == "parent-after-preflight" {
        args.iter().any(|s| s == "merge-tree")
    } else if mode == "worker-before-clean" || mode == "parent-before-clean" {
        args.iter().any(|s| s == "--ignored=matching")
    } else {
        false
    };
    if trigger
        && fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(env::var("LANE_TEST_MARKER").unwrap())
            .is_ok()
    {
        let path = std::path::Path::new(&target).join("late.txt");
        fs::write(&path, format!("late unverified work {mode}\n")).unwrap();
        for options in [vec!["add", "late.txt"], vec!["commit", "-m", "late work"]] {
            assert!(
                Command::new(&real)
                    .arg("-C")
                    .arg(&target)
                    .args(options)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
    io::stdout().write_all(&output.stdout).unwrap();
    io::stderr().write_all(&output.stderr).unwrap();
    std::process::exit(output.status.code().unwrap_or(130));
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    fn AttachConsole(pid: u32) -> i32;
    fn FreeConsole() -> i32;
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}
#[cfg(windows)]
unsafe extern "system" fn ignore_event(_: u32) -> i32 {
    1
}
#[cfg(unix)]
unsafe extern "C" {
    fn getppid() -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}
