//! Windows: gives freemonplay-host.exe its icon, version details and a manifest (modern
//! common controls for the setup dialog, and per-monitor DPI awareness).

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="freemonplay.host" version="1.0.0.0"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=../../build/windows/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    // Icon id 1: the tray icon and the setup dialog load it from here.
    res.set_icon_with_id("../../build/windows/icon.ico", "1")
        .set_manifest(MANIFEST)
        .set("ProductName", "freemonplay")
        .set("FileDescription", "freemonplay host")
        .set("LegalCopyright", "Copyright (C) 2026 moridin1138. GPL-3.0-or-later.");
    // The setup dialog needs the manifest (TaskDialogIndirect is only in Common Controls 6),
    // so unlike the client a missing resource compiler is an error here.
    res.compile().expect("couldn't add the Windows resources (icon and manifest)");
}
