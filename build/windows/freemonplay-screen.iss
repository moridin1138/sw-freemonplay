; Inno Setup script for the Windows client (a "screen"). CI compiles it with:
;   iscc /DVersion=0.2.0 /DSource=..\..\target\release build\windows\freemonplay-screen.iss
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
AppName=Freemonplay Screen
AppVersion={#Version}
AppVerName=Freemonplay Screen {#Version}
AppPublisher=moridin1138
AppPublisherURL=https://github.com/moridin1138/sw-freemonplay
DefaultDirName={localappdata}\Programs\freemonplay
DefaultGroupName=Freemonplay
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=freemonplay-screen-windows-x64-setup
OutputDir=..\..\dist
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\freemonplay-screen.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Close a running copy before replacing it.
CloseApplications=yes

#include "languages.iss"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\freemonplay-screen.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[InstallDelete]
; Before 0.3 the program was freemonplay-client.exe.
Type: files; Name: "{app}\freemonplay-client.exe"

[Icons]
Name: "{group}\Freemonplay Screen"; Filename: "{app}\freemonplay-screen.exe"; Comment: "{cm:ScreenComment}"
Name: "{group}\Uninstall Freemonplay Screen"; Filename: "{uninstallexe}"
Name: "{userdesktop}\Freemonplay Screen"; Filename: "{app}\freemonplay-screen.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\freemonplay-screen.exe"; Description: "{cm:LaunchProgram,Freemonplay Screen}"; Flags: nowait postinstall skipifsilent
