; Inno Setup script for the Windows host (the PC that gets extra screens). CI compiles it with:
;   iscc /DVersion=0.2.0 /DSource=..\..\target\release /DDriver=..\..\vdd\VirtualDisplayDriver build\windows\freemonplay-host.iss
; where Driver is the unpacked VirtualDisplayDriver-x86.Driver.Only.zip (MIT licensed,
; signed by the SignPath Foundation) with its LICENSE.txt added.
; Installing the display driver and the firewall rule needs administrator rights, so this
; installer asks for them once; the tray app itself runs as the signed-in user.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\target\release"
#endif
#ifndef Driver
  #define Driver "..\..\vdd\VirtualDisplayDriver"
#endif

[Setup]
AppId={{2B7D9E41-5C3A-4F86-A1E0-7D4C2B9F6E13}
AppName=freemonplay host
AppVersion={#Version}
AppVerName=freemonplay host {#Version}
AppPublisher=moridin1138
AppPublisherURL=https://github.com/moridin1138/sw-freemonplay
DefaultDirName={autopf}\freemonplay host
DefaultGroupName=freemonplay
DisableProgramGroupPage=yes
PrivilegesRequired=admin
OutputBaseFilename=freemonplay-host-windows-x64-setup
OutputDir=..\..\dist
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\freemonplay-host.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Close a running copy before replacing it (it removes its virtual monitors on the way out).
CloseApplications=force
; The "start at sign-in" task writes the user's own Run key.
UsedUserAreasWarning=no

[Tasks]
Name: "startup"; Description: "Start freemonplay when I sign in"

[Files]
Source: "{#Source}\freemonplay-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Driver}\*"; DestDir: "{app}\driver"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{group}\freemonplay host"; Filename: "{app}\freemonplay-host.exe"; Comment: "Give this PC extra screens"
Name: "{group}\Uninstall freemonplay host"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "freemonplay"; ValueData: """{app}\freemonplay-host.exe"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\freemonplay-host.exe"; Parameters: "--install-display-driver ""{app}\driver"" --allow-firewall"; StatusMsg: "Installing the display driver and the firewall rule..."; Flags: runhidden waituntilterminated
Filename: "{app}\freemonplay-host.exe"; Description: "Start freemonplay"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\freemonplay-host.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "StopHost"
Filename: "{app}\freemonplay-host.exe"; Parameters: "--uninstall-display-driver --remove-firewall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveDriver"
