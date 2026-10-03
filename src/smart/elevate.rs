//! Running the SMART reader with administrator rights.

use super::{DriveReading, HELPER_FLAG};

#[cfg(windows)]
pub fn is_elevated() -> bool {
    // SAFETY: no arguments.
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Re-launches this executable elevated as the SMART helper for `letters`
/// (triggering a UAC prompt), waits for it and reads back what it found.
#[cfg(windows)]
pub fn run_helper(letters: &[String]) -> Result<Vec<DriveReading>, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    let exe = std::env::current_exe().map_err(|e| format!("Couldn't locate dupe-rs: {e}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let out =
        std::env::temp_dir().join(format!("dupe-rs-smart-{}-{nanos}.json", std::process::id()));
    let params = format!("{HELPER_FLAG} \"{}\" {}", out.display(), letters.join(" "));

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&params);
    // SAFETY: plain-old-data; all-zero is the documented starting state.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // SW_HIDE

    // SAFETY: `info` and the strings it points to outlive the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let err = std::io::Error::last_os_error();
        return Err(if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            "Reading drive health needs administrator rights; the prompt was declined.".into()
        } else {
            format!("Couldn't start the health check: {err}")
        });
    }
    let mut exit_code = 1u32;
    // SAFETY: SEE_MASK_NOCLOSEPROCESS gives us the process handle to wait
    // on and close.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut exit_code);
        CloseHandle(info.hProcess);
    }

    let result = std::fs::read(&out);
    let _ = std::fs::remove_file(&out);
    let bytes = result
        .map_err(|e| format!("The health check didn't finish (exit code {exit_code}): {e}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("Couldn't read the health check's results: {e}"))
}

#[cfg(not(windows))]
pub fn run_helper(_letters: &[String]) -> Result<Vec<DriveReading>, String> {
    Err("Reading drive health is only supported on Windows".into())
}
