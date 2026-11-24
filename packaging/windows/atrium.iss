; Per-user Atrium installer for x64 Windows.
; script/bundle-windows.ps1 passes the version and the built binaries.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef AppExe
  #error AppExe must point at the built atrium.exe
#endif
#ifndef NodeExe
  #error NodeExe must point at the built atrium-node.exe
#endif

[Setup]
AppId={{7C1F9B2A-4E3D-4A8B-9C6E-5F0D1A2B3C4D}
AppName=Atrium
AppVersion={#AppVersion}
AppPublisher=Atrium
AppPublisherURL=https://github.com/J-X-C-Z/Atrium
AppSupportURL=https://github.com/J-X-C-Z/Atrium
DefaultDirName={localappdata}\Programs\Atrium
DefaultGroupName=Atrium
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputDir=..\..\dist
OutputBaseFilename=Atrium-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\atrium.exe
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; DestName: "atrium.exe"; Flags: ignoreversion
Source: "{#NodeExe}"; DestDir: "{app}"; DestName: "atrium-node.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\Atrium"; Filename: "{app}\atrium.exe"
Name: "{group}\Uninstall Atrium"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Atrium"; Filename: "{app}\atrium.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\atrium.exe"; Description: "Launch Atrium"; Flags: nowait postinstall skipifsilent
