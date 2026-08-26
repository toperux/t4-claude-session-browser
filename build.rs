//! Embeds the app icon and version metadata into the Windows executables, so
//! Explorer, the taskbar and the installer's Start Menu shortcut all show it.
//! No-op everywhere else.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/csb.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/csb.ico");
        res.set("FileDescription", "Claude Session Browser");
        res.set("ProductName", "Claude Session Browser");
        res.set(
            "LegalCopyright",
            "Copyright (c) 2026 Christopher Montevirgen",
        );
        if let Err(e) = res.compile() {
            // Don't fail the build over decoration - a missing resource
            // compiler should cost the icon, not the binary.
            println!("cargo:warning=icon embedding skipped: {e}");
        }
    }
}
