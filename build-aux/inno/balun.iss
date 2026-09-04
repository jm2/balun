; Balun — Inno Setup installer script
; Used by: scripts/build-windows.ps1 -InnoSetup
;
; The installer copies the probed, validated dist tree as-is. It keeps the
; MSYS2 prefix shape the application was built and probed against:
; bin\balun.exe beside its DLLs, lib\gstreamer-1.0 for the plugin closure, and
; libexec\gstreamer-1.0 for the plugin scanner, so GStreamer and GLib locate
; everything from their own DLL location without an environment variable.
;
; Preprocessor defines (passed via /D on the iscc command line):
;   AppVersion        — the Cargo package version, e.g. "0.1.0-alpha.1"
;   AppNumericVersion — its four-part numeric form for the Windows version
;                       resource, e.g. "0.1.0.0"
;   SourceDir         — path to the bundled dist folder (dist\balun-windows)
;   OutputDir         — where to write the installer exe
;   TargetArch        — "x64" or "arm64"

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef AppNumericVersion
  #define AppNumericVersion "0.1.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\balun-windows"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#ifndef TargetArch
  #define TargetArch "x64"
#endif

[Setup]
AppName=Balun
AppVersion={#AppVersion}
AppVerName=Balun
; Deterministic application GUID: UUID version 5 of the URL namespace and
; https://github.com/jm2/balun/io.github.jm2.Balun. It must never change and
; must never reuse another application's GUID.
AppId={{3B7A0CD1-33F6-5D60-9973-2B7A1B53E02A}
VersionInfoVersion={#AppNumericVersion}
VersionInfoProductVersion={#AppNumericVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoProductName=Balun
VersionInfoDescription=Balun Setup
VersionInfoCopyright=Copyright (C) 2026 Balun Contributors
AppPublisher=Balun Contributors
AppPublisherURL=https://github.com/jm2/balun
AppSupportURL=https://github.com/jm2/balun/issues
AppUpdatesURL=https://github.com/jm2/balun/releases
DefaultDirName={autopf}\Balun
DefaultGroupName=Balun
UninstallDisplayIcon={app}\bin\balun.exe
OutputDir={#OutputDir}
OutputBaseFilename=balun-setup
Compression=lzma2/ultra64
SolidCompression=yes
SetupIconFile=..\..\data\balun.ico
LicenseFile=..\..\LICENSE
WizardStyle=modern
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline
; Silent install support (Winget passes /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-)
DisableDirPage=auto
DisableProgramGroupPage=auto
CloseApplications=yes
CloseApplicationsFilter=balun.exe
SetupLogging=yes
#if TargetArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#elif TargetArch == "x64"
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#else
  #error Unsupported TargetArch; expected x64 or arm64
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; Copy everything from the bundled dist directory
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Balun"; Filename: "{app}\bin\balun.exe"
Name: "{group}\Uninstall Balun"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Balun"; Filename: "{app}\bin\balun.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\balun.exe"; Description: "Launch Balun"; Flags: nowait postinstall skipifnotsilent
