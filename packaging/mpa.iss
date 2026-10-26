; MyPaper Print Agent — instalador clásico "siguiente, siguiente, siguiente".
;
; Compilado por packaging/build.ps1, que deja en su sitio los binarios y las
; dependencias:
;   target\x86_64-pc-windows-msvc\release\mpa.exe           (CLI: pair, doctor, entitlement)
;   target\x86_64-pc-windows-msvc\release\mpa-desktop.exe   (ventana Tauri)
;   vendor\pdfium\pdfium.dll                                (rasterizador PDF, BSD-3)
;   vendor\webview2\...RuntimeInstallerX64.exe              (offline, opcional)
;
; Qué verifica antes de tocar nada:
;   1. Windows 10 1809 (build 17763) o superior, x64.
;   2. Que pdfium.dll esté en el paquete; sin él no hay análisis de PDF.
;   3. WebView2 Runtime; si falta, se instala desde el paquete offline.
;
; Nada de esto pide decisiones al usuario: tres clics y un resumen final.

#define MyAppName "MyPaper Print Agent"
#ifndef MyAppVersion
  #define MyAppVersion "0.3.0"
#endif
#define MyAppExe "mpa-desktop.exe"

; Ruta de los binarios. build.ps1 pasa la absoluta; por defecto, la del host.
#ifndef TargetDir
  #define TargetDir "..\target\release"
#endif
#define MyAppId "{{8F3A1C2B-4D5E-4F6A-9A1B-2C3D4E5F6071}"

; El instalador offline de WebView2 pesa ~127 MB. Si build.ps1 no lo descargó,
; el paquete se genera igual y la comprobación avisa en vez de fallar.
#define WebView2Offline "..\vendor\webview2\MicrosoftEdgeWebview2RuntimeInstallerX64.exe"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=MyPaper
DefaultDirName={autopf}\MyPaperPrintAgent
DefaultGroupName={#MyAppName}
OutputBaseFilename=mpa-{#MyAppVersion}-setup
; build.ps1 lee Output\mpa-<version>-setup.exe desde la raíz del repo.
OutputDir=..\Output
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; El agente lee %SystemRoot%\System32\spool\PRINTERS: sin administrador no hay
; análisis. El instalador también lo necesita para dejar la tarea programada.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName}
DisableProgramGroupPage=yes

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[InstallDelete]
; El shell de gpui se retiró: si quedó de una versión anterior, estorba.
Type: files; Name: "{app}\mpa-ui.exe"

[Files]
Source: "{#TargetDir}\mpa.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TargetDir}\mpa-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\vendor\pdfium\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
#if FileExists(AddBackslash(SourcePath) + WebView2Offline)
Source: "{#WebView2Offline}"; DestDir: "{tmp}"; Flags: dontcopy deleteafterinstall
#endif

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Crear un icono en el escritorio"; Flags: unchecked
Name: "autostart"; Description: "Abrir al iniciar Windows"; Flags: checkedonce

[Registry]
; Sin entrada HKCU\...\Run: el agente exige administrador y el Run key abriría
; un UAC en cada inicio de sesión. La tarea programada de [Run] arranca elevada.

[Run]
Filename: "{app}\mpa.exe"; Parameters: "autostart on"; Tasks: autostart; \
  Flags: runhidden waituntilterminated
Filename: "{app}\{#MyAppExe}"; Description: "Abrir {#MyAppName} ahora"; \
  Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\mpa.exe"; Parameters: "autostart off"; Flags: runhidden waituntilterminated; \
  RunOnceId: "RemoveAutostart"

[Code]
const
  WEBVIEW2_CLIENT = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

function WebView2Installed: Boolean;
var
  Version: String;
begin
  { Instalación por máquina (vista de 64 y de 32 bits) o por usuario. }
  Result :=
    RegQueryStringValue(HKLM64, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\' + WEBVIEW2_CLIENT, 'pv', Version) or
    RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\' + WEBVIEW2_CLIENT, 'pv', Version) or
    RegQueryStringValue(HKCU, 'Software\Microsoft\EdgeUpdate\Clients\' + WEBVIEW2_CLIENT, 'pv', Version);
end;

function InitializeSetup: Boolean;
begin
  Result := True;
  { El paquete se genera con pdfium.dll al lado; si falta, el instalador
    quedaría "correcto" y el agente sin poder analizar nada. }
  if not FileExists(ExpandConstant('{src}\pdfium.dll')) then
  begin
    MsgBox('El paquete está incompleto: falta pdfium.dll.' + #13#10 +
           'Vuelve a generarlo con packaging\build.ps1.', mbError, MB_OK);
    Result := False;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Installer: String;
begin
  if CurStep <> ssInstall then Exit;
  if WebView2Installed then Exit;

  Installer := ExpandConstant('{tmp}\MicrosoftEdgeWebview2RuntimeInstallerX64.exe');
  if not FileExists(Installer) then
  begin
    { Sin paquete offline no se puede resolver en silencio. Se avisa y se sigue:
      Windows 10/11 recientes suelen traerlo, y el usuario puede instalarlo
      después sin reinstalar el agente. }
    MsgBox('No se encontró WebView2 Runtime y el paquete no incluye el instalador offline.' + #13#10 +
           'La ventana no abrirá hasta instalarlo desde https://developer.microsoft.com/microsoft-edge/webview2/',
           mbInformation, MB_OK);
    Exit;
  end;

  WizardForm.StatusLabel.Caption := 'Instalando WebView2 Runtime…';
  if (not Exec(Installer, '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode)) or
     (ResultCode <> 0) then
    MsgBox('WebView2 Runtime no se pudo instalar (código ' + IntToStr(ResultCode) + ').' + #13#10 +
           'La ventana del agente no abrirá hasta que esté instalado.', mbError, MB_OK);
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  { Las actualizaciones silenciosas (/VERYSILENT) no deben robar el foco. }
  Result := WizardSilent();
end;