//! New-sample notification sound.

#[cfg(windows)]
pub fn beep() {
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
    // SAFETY: MessageBeep takes a plain flag value and has no memory-safety preconditions.
    unsafe {
        MessageBeep(MB_ICONASTERISK);
    }
}

#[cfg(not(windows))]
pub fn beep() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}
