//! Preserve the single-line interrupted response without a background runtime.
use serde_json::json;
use std::sync::OnceLock;
static RESPONSE: OnceLock<Vec<u8>> = OnceLock::new();

pub fn install(command: Option<&str>) {
    let mut bytes=crate::util::ascii_json(&json!({"format":"lane-agent-response/v1","ok":false,"command":command,"error":{"code":"interrupted","message":"operation interrupted"}})).into_bytes();
    bytes.push(b'\n');
    let _ = RESPONSE.set(bytes);
    #[cfg(windows)]
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
    #[cfg(unix)]
    unsafe {
        signal(2, handler as *const () as usize);
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
    fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
    fn WriteFile(
        handle: *mut std::ffi::c_void,
        bytes: *const u8,
        size: u32,
        written: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn ExitProcess(code: u32) -> !;
}
#[cfg(windows)]
unsafe extern "system" fn handler(event: u32) -> i32 {
    if event > 1 {
        return 0;
    }
    if let Some(bytes) = RESPONSE.get() {
        let mut written = 0;
        unsafe {
            WriteFile(
                GetStdHandle(-11i32 as u32),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }
    unsafe { ExitProcess(130) }
}
#[cfg(unix)]
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
    fn write(fd: i32, buf: *const u8, len: usize) -> isize;
    fn _exit(code: i32) -> !;
}
#[cfg(unix)]
extern "C" fn handler(_: i32) {
    if let Some(bytes) = RESPONSE.get() {
        unsafe {
            write(1, bytes.as_ptr(), bytes.len());
        }
    }
    unsafe { _exit(130) }
}
