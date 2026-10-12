; Per-user installer for the native build.
;
; It installs into the same folder, uninstall key and Start menu shortcut as the
; previous Tauri NSIS installer, so installed users upgrade in place. The old
; uninstaller is replaced rather than run: it could offer to delete the profile
; data that the native app migrates from. Settings, history and models live in
; %APPDATA% / %LOCALAPPDATA%\com.annotate.app and are never touched here.
;
; makensis /DVERSION=x.y.z /DSOURCE_DIR=<package folder> /DICON=<icon.ico> /DOUTFILE=<setup.exe> installer.nsi

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "LogicLib.nsh"

!define PRODUCT "Annotate"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
; Written by the Tauri installer; kept so the previous install folder is reused.
!define DIR_KEY "Software\annotate\${PRODUCT}"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT}"
InstallDirRegKey HKCU "${DIR_KEY}" ""
BrandingText "${PRODUCT} ${VERSION}"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\annotate.exe"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} Setup"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"

!macro CloseRunningApp
  ; Both the Tauri and native builds are named annotate.exe.
  nsExec::ExecToStack 'cmd /c tasklist /FI "IMAGENAME eq annotate.exe" /NH | find /I "annotate.exe"'
  Pop $0
  Pop $1
  ${If} $0 == 0
    MessageBox MB_OKCANCEL|MB_ICONINFORMATION "Annotate is running and will be closed to continue." IDOK +2
    Abort
    nsExec::Exec 'taskkill /IM annotate.exe /F'
    Sleep 500
  ${EndIf}
!macroend

Function .onInit
  ; The Tauri MSI installs per machine and cannot be replaced from a per-user setup.
  SetRegView 64
  StrCpy $1 0
  ${Do}
    EnumRegKey $0 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall" $1
    ${If} $0 == ""
      ${Break}
    ${EndIf}
    ReadRegStr $2 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$0" "DisplayName"
    ReadRegDWORD $3 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$0" "WindowsInstaller"
    ${If} $2 == "${PRODUCT}"
    ${AndIf} $3 == 1
      MessageBox MB_OK|MB_ICONINFORMATION "An older Annotate MSI installation was found.$\r$\n$\r$\nAfter setup finishes, remove it from Settings > Apps > Installed apps. Your settings, dictionary and history are kept."
      ${Break}
    ${EndIf}
    IntOp $1 $1 + 1
  ${Loop}
  SetRegView default
FunctionEnd

Section "Install"
  !insertmacro CloseRunningApp
  SetOutPath "$INSTDIR"
  ; The CUDA worker from Tauri releases is no longer used.
  Delete "$INSTDIR\whisper-worker.exe"
  File "${SOURCE_DIR}\annotate.exe"
  File "${SOURCE_DIR}\DirectML.dll"
  File "${SOURCE_DIR}\whisper-worker-vulkan.exe"
  File /nonfatal "${SOURCE_DIR}\onnxruntime.dll"
  File /nonfatal "${SOURCE_DIR}\onnxruntime_providers_shared.dll"
  File "${SOURCE_DIR}\README.md"
  File "${SOURCE_DIR}\Satoshi-FFL.txt"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\annotate.exe"

  WriteRegStr HKCU "${DIR_KEY}" "" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "annotate"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "MainBinaryName" "annotate.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" '"$INSTDIR\annotate.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" '"$INSTDIR"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  !insertmacro CloseRunningApp
  Delete "$INSTDIR\annotate.exe"
  Delete "$INSTDIR\DirectML.dll"
  Delete "$INSTDIR\whisper-worker-vulkan.exe"
  Delete "$INSTDIR\whisper-worker.exe"
  Delete "$INSTDIR\onnxruntime.dll"
  Delete "$INSTDIR\onnxruntime_providers_shared.dll"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Satoshi-FFL.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "${DIR_KEY}"
  DeleteRegKey /ifempty HKCU "Software\annotate"
SectionEnd
