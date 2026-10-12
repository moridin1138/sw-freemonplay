//! Windows: gives the .exe its icon and version details (shown in Explorer, the taskbar
//! and the installer's uninstall entry).

fn main() {
    println!("cargo:rerun-if-changed=../../build/windows/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../build/windows/icon.ico")
        .set("ProductName", "Freemonplay")
        .set("FileDescription", "Freemonplay Screen")
        .set("LegalCopyright", "Copyright (C) 2026 moridin1138. GPL-3.0-or-later.");
    if let Err(e) = res.compile() {
        // A missing resource compiler shouldn't stop the build; the .exe just has no icon.
        println!("cargo:warning=couldn't add the Windows icon: {e}");
    }
}
