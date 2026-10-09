; Windows installer of eoview (Inno Setup). For the user, without administrator rights: the automatic
; update (crates/eoview/src/update.rs) can then replace eoview.exe. .github/workflows/release.yml builds it:
; ISCC /DVersion=X.Y.Z packaging\eoview.iss
#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{E121EB82-9CF7-49CE-ACAE-CCB12BB24BB4}
AppName=eoview
AppVersion={#Version}
AppPublisher=polymood
AppPublisherURL=https://github.com/polymood/eoview
DefaultDirName={autopf}\eoview
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\out
OutputBaseFilename=eoview-setup-windows-x86_64
SetupIconFile=..\crates\eoview\assets\eoview.ico
UninstallDisplayIcon={app}\eoview.exe
WizardStyle=modern
CloseApplications=yes
ChangesAssociations=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "..\out\eoview-windows-x86_64.exe"; DestDir: "{app}"; DestName: "eoview.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\eoview"; Filename: "{app}\eoview.exe"
Name: "{autodesktop}\eoview"; Filename: "{app}\eoview.exe"; Tasks: desktopicon

; The workspace files (.eoview) open in eoview.
[Registry]
Root: HKA; Subkey: "Software\Classes\.eoview"; ValueType: string; ValueName: ""; ValueData: "eoview.workspace"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\eoview.workspace"; ValueType: string; ValueName: ""; ValueData: "eoview workspace"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\eoview.workspace\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\eoview.exe,0"
Root: HKA; Subkey: "Software\Classes\eoview.workspace\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\eoview.exe"" ""%1"""

[Run]
Filename: "{app}\eoview.exe"; Description: "{cm:LaunchProgram,eoview}"; Flags: nowait postinstall skipifsilent

; The files of the automatic update.
[UninstallDelete]
Type: files; Name: "{app}\eoview.old"
Type: files; Name: "{app}\eoview.new"
