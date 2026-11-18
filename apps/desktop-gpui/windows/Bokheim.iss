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
ChangesAssociations=yes

[Files]
Source: "{#ExtractFilePath(SourceExe)}fonts\*"; DestDir: "{app}\fonts"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#ExtractFilePath(SourceExe)}taxonomy.sqlite3"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "Bokheim.exe"; Flags: ignoreversion
Source: "{#ExtractFilePath(SourceExe)}bokheim-browser.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ExtractFilePath(SourceExe)}tts\*"; DestDir: "{app}\tts"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#IconFile}"; DestDir: "{app}"; DestName: "Bokheim.ico"; Flags: ignoreversion

[Icons]
Name: "{group}\Bokheim"; Filename: "{app}\Bokheim.exe"; WorkingDir: "{app}"; IconFilename: "{app}\Bokheim.ico"

[Registry]
Root: HKCU; Subkey: "Software\Classes\Bokheim.Book"; ValueType: string; ValueData: "Bokheim book"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Bokheim.Book\DefaultIcon"; ValueType: string; ValueData: "{app}\Bokheim.ico"
Root: HKCU; Subkey: "Software\Classes\Bokheim.Book\shell\open\command"; ValueType: string; ValueData: """{app}\Bokheim.exe"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.epub\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.mobi\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.azw\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.azw3\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.fb2\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.fbz\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.cbz\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\.m4b\OpenWithProgids"; ValueType: none; ValueName: "Bokheim.Book"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\Bokheim.exe"; Description: "Launch Bokheim"; Flags: nowait postinstall skipifsilent
