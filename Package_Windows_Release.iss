; PineFlash CE installer (Inno Setup 6).
; Build from the repo root after `cargo build --release` and scripts\fetch-windows-tools.ps1:
;   ISCC Package_Windows_Release.iss
; The GitHub workflow passes the version from Cargo.toml: ISCC /DMyAppVersion=x.y.z ...
; For the Linux cross-build used by generate-release.sh:
;   ISCC /DBuildDir=target\x86_64-pc-windows-gnu\release Package_Windows_Release.iss

#define MyAppName "PineFlash CE"
#ifndef MyAppVersion
  #define MyAppVersion "0.7.0"
#endif
#define MyAppPublisher "GRIMREAPER35487"
#define MyAppURL "https://github.com/GRIMREAPER35487/pineflash-CE"
#define MyAppExeName "pineflash.exe"
#ifndef BuildDir
  #define BuildDir "target\release"
#endif

[Setup]
; New AppId: PineFlash CE installs alongside the original PineFlash instead of replacing it.
AppId={{B4D0DFEF-B8E7-4111-9E9A-2FD6DF48F0C0}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppComments=Community continuation of PineFlash by Spagett1
; pineflash.exe is 64-bit: install into "Program Files", not "Program Files (x86)".
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
LicenseFile={#SourcePath}LICENSE
; Uncomment the following line to run in non administrative install mode (install for current user only.)
;PrivilegesRequired=lowest
OutputDir={#SourcePath}
OutputBaseFilename=PineFlashCE_Installer
SetupIconFile={#SourcePath}assets\pine64logo.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourcePath}{#BuildDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
; dfu-util for the Pinecil V1 (the Pinecil V2 uses the built-in flasher on Windows).
Source: "{#SourcePath}tools\win\*"; DestDir: "{app}\tools\"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
; NOTE: Don't use "Flags: ignoreversion" on any shared system files

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
