!define PRODUCT_NAME "zing"
!define PRODUCT_PUBLISHER "TharukRenuja"
!define PRODUCT_VERSION "0.1.3"
!ifndef SOURCE_DIR
!define SOURCE_DIR "."
!endif
!ifndef ARCH_SUFFIX
!define ARCH_SUFFIX "x86_64-windows"
!endif

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "WinMessages.nsh"

; Modern UI pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_LANGUAGE "English"

Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
OutFile "zing-${PRODUCT_VERSION}-${ARCH_SUFFIX}-installer.exe"
InstallDir "$PROGRAMFILES64\${PRODUCT_NAME}"
InstallDirRegKey HKLM "Software\${PRODUCT_PUBLISHER}\${PRODUCT_NAME}" "InstallDir"

RequestExecutionLevel admin

Section "Install"
  SetOutPath "$INSTDIR"

  ; Copy binaries
  File "${SOURCE_DIR}\zing.exe"
  File "${SOURCE_DIR}\zing-daemon.exe"

  ; Write uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Add to system PATH
  Push "$INSTDIR"
  Call AddToPath

  ; Uninstall registry keys
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "InstallLocation" "$INSTDIR"
  WriteRegDword HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "NoModify" 1
  WriteRegDword HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
    "NoRepair" 1

  ; Save install dir for upgrade detection
  WriteRegStr HKLM "Software\${PRODUCT_PUBLISHER}\${PRODUCT_NAME}" "InstallDir" "$INSTDIR"
SectionEnd

Section "Uninstall"
  ; Remove from PATH
  Push "$INSTDIR"
  Call un.RemoveFromPath

  ; Remove files
  Delete "$INSTDIR\zing.exe"
  Delete "$INSTDIR\zing-daemon.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ; Remove registry keys
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
  DeleteRegKey HKLM "Software\${PRODUCT_PUBLISHER}\${PRODUCT_NAME}"
SectionEnd

; Helper: add directory to system PATH
Function AddToPath
  Exch $0 ; dir to add
  Push $1
  Push $2

  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "PATH"

  ; Check if already in PATH
  Push "$0;"
  Push "$1"
  Call StrStr
  Pop $2
  ${If} $2 == ""
    ${If} $1 != ""
      StrCpy $1 "$1;$0"
    ${Else}
      StrCpy $1 "$0"
    ${EndIf}
    WriteRegStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "PATH" "$1"
    ; Notify system of environment change
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment"
  ${EndIf}

  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; Helper: remove directory from system PATH
Function un.RemoveFromPath
  Exch $0 ; dir to remove
  Push $1
  Push $2
  Push $3
  Push $4

  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "PATH"
  ${If} $1 == ""
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Return
  ${EndIf}

  ; Remove dir; from path
  Push "$0;"
  Push "$1"
  Call un.StrStr
  Pop $2
  ${If} $2 != ""
    StrCpy $3 "$1$2" "" 0 ; prefix before match
    StrLen $4 "$0"
    IntOp $4 $4 + 1 ; account for semicolon
    StrCpy $2 "$2" "" $4 ; suffix after dir;
    StrCpy $1 "$3$2"
    ; Clean up leading/trailing semicolons
    ${Do}
      StrCpy $2 "$1" 1
      ${If} $2 == ";"
        StrCpy $1 "$1" "" 1
      ${Else}
        ${Break}
      ${EndIf}
    ${Loop}
    WriteRegStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "PATH" "$1"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment"
  ${EndIf}

  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; StrStr: find first occurrence of substring
Function StrStr
  Exch $0 ; search for
  Exch 1
  Exch $1 ; search in
  Push $2
  Push $3

  StrCpy $2 0
  StrLen $3 $0
  ${If} $3 == 0
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Return
  ${EndIf}

  loop:
    StrCpy $2 $1 $3 $2
    ${If} $2 == ""
      StrCpy $0 ""
      Goto done
    ${EndIf}
    ${If} $2 == $0
      StrCpy $0 $1 "" $2
      Goto done
    ${EndIf}
    IntOp $2 $2 + 1
    Goto loop

  done:
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

Function un.StrStr
  Exch $0
  Exch 1
  Exch $1
  Push $2
  Push $3

  StrCpy $2 0
  StrLen $3 $0
  ${If} $3 == 0
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Return
  ${EndIf}

  loop_u:
    StrCpy $2 $1 $3 $2
    ${If} $2 == ""
      StrCpy $0 ""
      Goto done
    ${EndIf}
    ${If} $2 == $0
      StrCpy $0 $1 "" $2
      Goto done
    ${EndIf}
    IntOp $2 $2 + 1
    Goto loop_u

  done:
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd
