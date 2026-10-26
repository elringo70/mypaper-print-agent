; MyPaper Print Agent — classic next-next-next installer (Inno Setup 6).
; Built by packaging/build.ps1, which fills AppVersion from Cargo.
; pdfium ships next to the binaries (fetched by packaging/fetch-pdfium.ps1):
; BSD-3-Clause, redistributable, and pinned so measurements stay reproducible.
; The installer verifies it is present and speaks up if it is not.

#define MyAppName "MyPaper Print Agent"
#ifndef AppVersion
  #define AppVersion "0.2.0"
#endif
#define MyAppExe "mpa-ui.exe"

[Setup]
AppId={{8F3A1C2B-4D5E-4F6A-9A1B-2C3D4E5F6071}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppPublisher=MyPaper
DefaultDirName={autopf}\MyPaperPrintAgent
DefaultGroupName={#MyAppName}
OutputBaseFilename=mpa-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; The app itself needs elevation (it reads the spool folder to analyze held
; jobs), so the installer runs elevated too: that lets it install under
; Program Files and register the elevated logon task below.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#MyAppName}

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[Files]
Source: "..\target\x86_64-pc-windows-msvc\release\mpa.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\mpa-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\vendor\pdfium\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Crear un icono en el escritorio"; Flags: unchecked
Name: "autostart"; Description: "Abrir al iniciar Windows"; Flags: checkedonce

[Registry]
; No HKCU\...\Run entry: mpa-ui.exe requires administrator, so the Run key
; would pop a UAC prompt at every logon (and skip the agent if declined).
; The "autostart" task registers a logon Task Scheduler entry with highest
; privileges instead (see [Run] below).

[Run]
Filename: "{app}\mpa.exe"; Parameters: "autostart on"; Tasks: autostart; \
  Flags: runhidden waituntilterminated
Filename: "{app}\{#MyAppExe}"; Description: "Abrir {#MyAppName} ahora"; \
  Flags: nowait postinstall skipifsilent

[Code]
// Silent updates (/VERYSILENT from the in-app updater) must not steal focus.
function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := WizardSilent();
end;
