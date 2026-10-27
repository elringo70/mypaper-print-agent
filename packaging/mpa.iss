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

; Sin pdfium no hay análisis de PDF. Se comprueba aquí, al compilar, y no al
; instalar: en tiempo de instalación el DLL viaja dentro del paquete y no está
; al lado del setup, así que mirar ahí daba un falso "paquete incompleto".
#if !FileExists(AddBackslash(SourcePath) + "..\vendor\pdfium\pdfium.dll")
  #pragma error "falta vendor\pdfium\pdfium.dll: ejecuta packaging\fetch-pdfium.ps1"
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=MyPaper
DefaultDirName={autopf}\MyPaperPrintAgent
DefaultGroupName={#MyAppName}
#ifndef OutBase
  #define OutBase "mpa-" + MyAppVersion + "-setup"
#endif
OutputBaseFilename={#OutBase}
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
Source: "setup-agent.ps1"; DestDir: "{app}"; Flags: ignoreversion
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
; El binario lleva manifiesto requireAdministrator y Setup corre elevado: con
; CreateProcess (el defecto) Windows lo rechaza con error 740. shellexec pasa por
; ShellExecute y muestra el UAC que corresponde.
Filename: "{app}\{#MyAppExe}"; Description: "Abrir {#MyAppName} ahora"; \
  Flags: nowait postinstall skipifsilent shellexec

[UninstallRun]
Filename: "{app}\mpa.exe"; Parameters: "autostart off"; Flags: runhidden waituntilterminated; \
  RunOnceId: "RemoveAutostart"

[Code]
var
  RolesPage: TWizardPage;
  BwCombo, ColorCombo: TNewComboBox;

const
  WEBVIEW2_CLIENT = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

{ Las impresoras ya están instaladas: el agente no las crea, sólo necesita saber
  cuál imprime en blanco y negro y cuál en color. }
procedure LoadPrinters(Combo: TNewComboBox);
var
  TempFile: String;
  Lines: TArrayOfString;
  I: Integer;
  ResultCode: Integer;
begin
  TempFile := ExpandConstant('{tmp}\mpa-printers.txt');
  Exec(ExpandConstant('{cmd}'),
       '/c powershell -NoProfile -Command "Get-Printer | Select-Object -ExpandProperty Name" > "' + TempFile + '"',
       '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if LoadStringsFromFile(TempFile, Lines) then
  begin
    for I := 0 to GetArrayLength(Lines) - 1 do
    begin
      if Trim(Lines[I]) <> '' then
        Combo.Items.Add(Trim(Lines[I]));
    end;
  end;
  DeleteFile(TempFile);
end;

procedure InitializeWizard;
var
  Label1, Label2: TNewStaticText;
begin
  RolesPage := CreateCustomPage(wpSelectTasks, 'Impresoras',
    'MyPaper no instala impresoras: usa las que ya tienes configuradas en Windows.');

  Label1 := TNewStaticText.Create(RolesPage);
  Label1.Parent := RolesPage.Surface;
  Label1.Caption := 'Impresora para blanco y negro';
  Label1.Top := 8;

  BwCombo := TNewComboBox.Create(RolesPage);
  BwCombo.Parent := RolesPage.Surface;
  BwCombo.Style := csDropDownList;
  BwCombo.Top := Label1.Top + Label1.Height + 6;
  BwCombo.Width := RolesPage.SurfaceWidth;
  LoadPrinters(BwCombo);

  Label2 := TNewStaticText.Create(RolesPage);
  Label2.Parent := RolesPage.Surface;
  Label2.Caption := 'Impresora para color';
  Label2.Top := BwCombo.Top + BwCombo.Height + 18;

  ColorCombo := TNewComboBox.Create(RolesPage);
  ColorCombo.Parent := RolesPage.Surface;
  ColorCombo.Style := csDropDownList;
  ColorCombo.Top := Label2.Top + Label2.Height + 6;
  ColorCombo.Width := RolesPage.SurfaceWidth;
  LoadPrinters(ColorCombo);

  { Sin elección no se puede medir nada: si hay dos impresoras, la segunda es la
    de color por defecto, que es como están montadas casi todas las papelerías. }
  if BwCombo.Items.Count > 0 then BwCombo.ItemIndex := 0;
  if ColorCombo.Items.Count > 1 then ColorCombo.ItemIndex := 1
  else if ColorCombo.Items.Count = 1 then ColorCombo.ItemIndex := 0;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = RolesPage.ID then
  begin
    if (BwCombo.ItemIndex < 0) or (ColorCombo.ItemIndex < 0) then
    begin
      MsgBox('Elige la impresora de blanco y negro y la de color.', mbError, MB_OK);
      Result := False;
    end
    else if BwCombo.Text = ColorCombo.Text then
    begin
      MsgBox('La misma impresora no puede ser la de blanco y negro y la de color: ' +
             'MyPaper necesita una para cada rol.', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function GetRoleParams(Param: String): String;
begin
  Result := '';
  if BwCombo.ItemIndex >= 0 then
    Result := Result + '-Bw "' + BwCombo.Text + '" ';
  if ColorCombo.ItemIndex >= 0 then
    Result := Result + '-Color "' + ColorCombo.Text + '"';
end;

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

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Installer: String;
begin
  { Antes de copiar nada: el runtime del webview, porque sin él la ventana no
    abre y el usuario se entera al final. }
  if CurStep = ssInstall then
  begin
    if WebView2Installed then Exit;

    Installer := ExpandConstant('{tmp}\MicrosoftEdgeWebview2RuntimeInstallerX64.exe');
    if not FileExists(Installer) then
    begin
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
    Exit;
  end;

  { Después de copiar: roles y cola IPP. Crear la cola hace que Windows pregunte
    al endpoint, así que el agente tiene que estar ya escuchando. }
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Configurando la cola MyPaper…';
    if (not Exec('powershell.exe',
                 '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\setup-agent.ps1') + '" ' + GetRoleParams(''),
                 '', SW_HIDE, ewWaitUntilTerminated, ResultCode)) or (ResultCode <> 0) then
      MsgBox('La cola MyPaper no quedó configurada (código ' + IntToStr(ResultCode) + ').' + #13#10 +
             'El agente está instalado: vuelve a ejecutar setup-agent.ps1 en su carpeta para terminar.',
             mbError, MB_OK);
  end;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  { Las actualizaciones silenciosas (/VERYSILENT) no deben robar el foco. }
  Result := WizardSilent();
end;