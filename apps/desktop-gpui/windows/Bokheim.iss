#ifndef MyAppVersion
  #error MyAppVersion must be provided by the release workflow
#endif
#ifndef SourceExe
  #error SourceExe must be provided by the release workflow
#endif
#ifndef OutputDir
  #error OutputDir must be provided by the release workflow
#endif
#ifndef IconFile
  #error IconFile must be provided by the release workflow
#endif

[Setup]
AppId={{9F09C6DC-E258-4AC2-B2D9-B82EA64D130C}
AppName=Bokheim
AppVersion={#MyAppVersion}
AppPublisher=Bokheim
DefaultDirName={localappdata}\Programs\Bokheim
DefaultGroupName=Bokheim
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Bokheim-Windows-x86_64-Setup
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\Bokheim.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "Bokheim.exe"; Flags: ignoreversion
Source: "{#IconFile}"; DestDir: "{app}"; DestName: "Bokheim.ico"; Flags: ignoreversion

[Icons]
Name: "{group}\Bokheim"; Filename: "{app}\Bokheim.exe"; WorkingDir: "{app}"; IconFilename: "{app}\Bokheim.ico"

[Run]
Filename: "{app}\Bokheim.exe"; Description: "Launch Bokheim"; Flags: nowait postinstall skipifsilent
