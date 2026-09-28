; Inno Setup script for the Windows installer. Built in CI:
;   iscc /DAppVersion=0.1.0 installer\genie.iss
; after `cargo build --release`. The installer lands in target\installer.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; Keep this AppId unchanged so upgrades replace the existing install.
AppId={{2E8FB702-ACB3-491A-8453-44008107D30D}
AppName=Genie
AppVersion={#AppVersion}
AppVerName=Genie {#AppVersion}
AppPublisher=Andrew Henshaw
AppPublisherURL=https://github.com/ahenshaw/genie
AppSupportURL=https://github.com/ahenshaw/genie/issues
DefaultDirName={autopf}\Genie
DefaultGroupName=Genie
DisableProgramGroupPage=yes
; Installs for the current user without admin rights, unless the user
; chooses "install for all users".
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=Genie-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\genie.exe
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "gedassoc"; Description: "Open .ged files with Genie"; GroupDescription: "File types:"; Flags: unchecked

[Files]
Source: "..\target\release\genie.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\fonts\Inter-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Genie"; Filename: "{app}\genie.exe"
Name: "{autodesktop}\Genie"; Filename: "{app}\genie.exe"; Tasks: desktopicon

[Registry]
; Always offer Genie under "Open with" for .ged files.
Root: HKA; Subkey: "Software\Classes\.ged\OpenWithProgids"; ValueType: string; ValueName: "Genie.GEDCOM"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Genie.GEDCOM"; ValueType: string; ValueName: ""; ValueData: "GEDCOM family tree"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Genie.GEDCOM\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\genie.exe,0"
Root: HKA; Subkey: "Software\Classes\Genie.GEDCOM\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\genie.exe"" ""%1"""
; .gdz bundles (a tree with its documents) are Genie's own export, so open them with Genie.
Root: HKA; Subkey: "Software\Classes\.gdz"; ValueType: string; ValueName: ""; ValueData: "Genie.Bundle"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Genie.Bundle"; ValueType: string; ValueName: ""; ValueData: "Genie family tree bundle"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Genie.Bundle\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\genie.exe,0"
Root: HKA; Subkey: "Software\Classes\Genie.Bundle\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\genie.exe"" ""%1"""
; Only when asked: make Genie the default for .ged.
Root: HKA; Subkey: "Software\Classes\.ged"; ValueType: string; ValueName: ""; ValueData: "Genie.GEDCOM"; Flags: uninsdeletevalue; Tasks: gedassoc

[Run]
Filename: "{app}\genie.exe"; Description: "{cm:LaunchProgram,Genie}"; Flags: nowait postinstall skipifsilent
