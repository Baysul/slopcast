Unicode true
!include "MUI2.nsh"
Name "Slopcast"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Slopcast"
RequestExecutionLevel user
SetCompressor /SOLID lzma
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section "Slopcast"
  SetOutPath "$INSTDIR"
  File "${BINARY}"
  File /oname=slopcast.ico "${ICON}"
  SetOutPath "$INSTDIR\licenses"
  File "${LICENSES}\*-OFL.txt"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Slopcast.lnk" "$INSTDIR\slopcast.exe" "" "$INSTDIR\slopcast.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slopcast" "DisplayName" "Slopcast"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slopcast" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slopcast" "UninstallString" '"$INSTDIR\uninstall.exe"'
SectionEnd
Section "Uninstall"
  Delete "$SMPROGRAMS\Slopcast.lnk"
  Delete "$INSTDIR\slopcast.exe"
  Delete "$INSTDIR\slopcast.ico"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$INSTDIR\licenses\*-OFL.txt"
  RMDir "$INSTDIR\licenses"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slopcast"
SectionEnd
