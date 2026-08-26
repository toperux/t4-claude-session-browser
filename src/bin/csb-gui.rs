//! The desktop face of `csb`, as its own GUI-subsystem executable.
//!
//! `csb.exe` is a console app, which is right for the CLI and TUI but means a
//! Start Menu shortcut would drag a console window along behind the GUI. This
//! binary exists purely so the installer has something console-free to point at;
//! it is the same `gui::run`, just without a console attached.

#![windows_subsystem = "windows"]

use claude_session_browser::{gui, paths::ClaudeDir};

fn main() {
    // No clap, and deliberately no `--version`: this binary has no console to
    // print to, so a version flag could only answer with a modal dialog, which
    // hangs anything scripting it. The version is in the exe's VERSIONINFO
    // resource instead - Explorer's Properties tab and
    // `(Get-Item csb-gui.exe).VersionInfo.FileVersion` both read it, and that
    // is what proves an update replaced this binary too. Use `csb --version`.
    if let Err(e) = run() {
        // A GUI-subsystem process has no stderr, so returning Err from main
        // would make a double-clicked shortcut do visibly nothing at all.
        report(&format!("csb could not start.\n\n{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let dir = ClaudeDir::resolve(None)?;
    gui::run(dir)
}

/// Show a message the user can actually see, with no console to print to.
#[cfg(windows)]
fn report(message: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, utype: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONINFORMATION: u32 = 0x0000_0040;

    let text = wide(message);
    let caption = wide("Claude Session Browser");
    // SAFETY: both pointers are to NUL-terminated UTF-16 buffers that outlive
    // the call, and a null owner handle means a message box with no parent.
    unsafe {
        MessageBoxW(
            0,
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
fn report(message: &str) {
    eprintln!("{message}");
}
