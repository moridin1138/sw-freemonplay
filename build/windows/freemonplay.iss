; Inno Setup script for the Windows client (a "screen"). CI compiles it with:
;   iscc /DVersion=0.2.0 /DSource=..\..\target\release build\windows\freemonplay.iss
; Installs for the current user only, so it never asks for administrator rights, and a
; newer installer simply updates in place.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\target\release"
#endif

[Setup]
AppId={{6F4C3A52-8E1D-4B7A-9C2F-3D5E8A1B4C70}
AppName=freemonplay
AppVersion={#Version}
AppVerName=freemonplay {#Version}
AppPublisher=moridin1138
AppPublisherURL=https://github.com/moridin1138/sw-freemonplay
DefaultDirName={localappdata}\Programs\freemonplay
DefaultGroupName=freemonplay
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=freemonplay-screen-windows-x64-setup
OutputDir=..\..\dist
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\freemonplay-client.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Close a running copy before replacing it.
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\freemonplay-client.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{group}\freemonplay Screen"; Filename: "{app}\freemonplay-client.exe"; Comment: "Use this computer as an extra screen for a Mac"
Name: "{group}\Uninstall freemonplay"; Filename: "{uninstallexe}"
Name: "{userdesktop}\freemonplay Screen"; Filename: "{app}\freemonplay-client.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\freemonplay-client.exe"; Description: "Start freemonplay Screen"; Flags: nowait postinstall skipifsilent
