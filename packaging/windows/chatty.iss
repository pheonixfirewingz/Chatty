; Inno Setup script for the Chatty desktop client.
; Built by scripts/build-windows.ps1, which passes /DAppVersion and /DStageDir.

#ifndef AppVersion
  #error AppVersion must be defined
#endif
#ifndef StageDir
  #error StageDir must be defined
#endif

[Setup]
AppId={{8C3E5A1D-6F2B-4E7A-9D41-3B0C2F7E9A64}
AppName=Chatty
AppVersion={#AppVersion}
AppPublisher=pheonixfirewingz
AppPublisherURL=https://github.com/pheonixfirewingz/Chatty
DefaultDirName={autopf}\Chatty
DefaultGroupName=Chatty
DisableProgramGroupPage=yes
; Per-user install by default; no administrator rights needed.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile={#StageDir}\LICENSE.txt
SetupIconFile={#StageDir}\chatty.ico
UninstallDisplayIcon={app}\chatty-gui.exe
OutputBaseFilename=chatty-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Chatty"; Filename: "{app}\chatty-gui.exe"; IconFilename: "{app}\chatty.ico"
Name: "{autodesktop}\Chatty"; Filename: "{app}\chatty-gui.exe"; IconFilename: "{app}\chatty.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\chatty-gui.exe"; Description: "{cm:LaunchProgram,Chatty}"; Flags: nowait postinstall skipifsilent
