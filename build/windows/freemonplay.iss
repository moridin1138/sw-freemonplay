; Inno Setup script for both Freemonplay roles. CI compiles it with:
;   iscc /DVersion=0.2.0 /DSource=..\..\target\release /DDriver=..\..\vdd\VirtualDisplayDriver build\windows\freemonplay.iss
; where Driver is the unpacked VirtualDisplayDriver-x86.Driver.Only.zip (MIT licensed,
; signed by the SignPath Foundation) with its LICENSE.txt added.
; The installer can upgrade the old host installation. The host setup guide installs
; the driver and firewall rule when they are needed.

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
AppName=Freemonplay
AppVersion={#Version}
AppVerName=Freemonplay {#Version}
AppPublisher=moridin1138
AppPublisherURL=https://github.com/moridin1138/sw-freemonplay
DefaultDirName={autopf}\Freemonplay
DefaultGroupName=Freemonplay
DisableProgramGroupPage=yes
PrivilegesRequired=admin
OutputBaseFilename=freemonplay-windows-x64-setup
OutputDir=..\..\dist
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\freemonplay-screen.exe
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

#include "languages.iss"

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"

[Files]
Source: "{#Source}\freemonplay-screen.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\freemonplay-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Driver}\*"; DestDir: "{app}\driver"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[InstallDelete]
Type: files; Name: "{group}\Freemonplay Host.lnk"
Type: files; Name: "{group}\Uninstall Freemonplay Host.lnk"

[Icons]
Name: "{group}\Freemonplay"; Filename: "{app}\freemonplay-screen.exe"
Name: "{group}\Uninstall Freemonplay"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "freemonplay"; ValueData: """{app}\freemonplay-host.exe"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\freemonplay-screen.exe"; Description: "{cm:LaunchProgram,Freemonplay}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\freemonplay-host.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "StopHost"
; The driver is optional and may be used by another app. Leave it installed.
Filename: "{app}\freemonplay-host.exe"; Parameters: "--remove-firewall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveFirewall"
