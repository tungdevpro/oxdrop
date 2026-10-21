; The Oxdrop installer for Windows 10 21H2 and later, one per architecture.
;
; Build with makensis, defining:
;   VERSION    the app version, such as 0.1.0
;   ARCH       x64 or arm64
;   BINARY     path to the built oxdrop.exe
;   WEBVIEW2   path to Microsoft's WebView2 bootstrapper (MicrosoftEdgeWebview2Setup.exe)
;   ROOT       path to the repository, for the license files
;   OUTFILE    where to write the installer
;
; It installs for the current user, so it needs no administrator rights, and installs
; the WebView2 runtime first if this PC doesn't have it yet.

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "WinVer.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

!define APP "Oxdrop"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
; The WebView2 runtime's id in EdgeUpdate.
!define WEBVIEW2_KEY "Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "LegalCopyright" "The Oxdrop contributors. MIT OR Apache-2.0."

!define MUI_FINISHPAGE_RUN "$INSTDIR\oxdrop.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP}"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
	InitPluginsDir
	; Windows 10 21H2 is build 19044.
	${IfNot} ${AtLeastWin10}
	${OrIfNot} ${AtLeastBuild} 19044
		MessageBox MB_OK|MB_ICONSTOP "${APP} needs Windows 10 version 21H2 or later."
		Abort
	${EndIf}
	!if ${ARCH} == "arm64"
		${IfNot} ${IsNativeARM64}
			MessageBox MB_OK|MB_ICONSTOP "This installer is for ARM64 PCs. Download the x64 installer instead."
			Abort
		${EndIf}
	!else
		${If} ${IsNativeARM64}
			MessageBox MB_OK|MB_ICONINFORMATION "This PC has an ARM64 processor. The ARM64 installer runs ${APP} faster; this one will still work."
		${EndIf}
	!endif
FunctionEnd

; Whether the WebView2 runtime is installed, per machine or for this user. Pushes 1 or 0.
Function HasWebView2
	Push $0
	; This installer is 32-bit, so HKLM\SOFTWARE reads the WOW6432Node view, which is
	; where a per-machine runtime registers itself.
	ReadRegStr $0 HKLM "SOFTWARE\${WEBVIEW2_KEY}" "pv"
	${If} $0 == ""
	${OrIf} $0 == "0.0.0.0"
		ReadRegStr $0 HKCU "Software\${WEBVIEW2_KEY}" "pv"
	${EndIf}
	${If} $0 == ""
	${OrIf} $0 == "0.0.0.0"
		StrCpy $0 0
	${Else}
		StrCpy $0 1
	${EndIf}
	Exch $0
FunctionEnd

Section "Install"
	SetOutPath "$INSTDIR"
	File "/oname=oxdrop.exe" "${BINARY}"
	File "/oname=LICENSE-MIT.txt" "${ROOT}\LICENSE-MIT"
	File "/oname=LICENSE-APACHE.txt" "${ROOT}\LICENSE-APACHE"

	Call HasWebView2
	Pop $0
	${If} $0 == 0
		DetailPrint "Installing the Microsoft WebView2 runtime..."
		File "/oname=$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2}"
		ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $1
		Call HasWebView2
		Pop $0
		${If} $0 == 0
			MessageBox MB_OK|MB_ICONEXCLAMATION "The Microsoft WebView2 runtime couldn't be installed (code $1). ${APP} needs it to show its window; install it from https://go.microsoft.com/fwlink/p/?LinkId=2124703 and start ${APP} again."
		${EndIf}
	${EndIf}

	CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\oxdrop.exe"
	WriteUninstaller "$INSTDIR\uninstall.exe"
	WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "The Oxdrop contributors"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\oxdrop.exe"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
	WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
	WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/tungdevpro/oxdrop"
	WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
	WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
	Delete "$SMPROGRAMS\${APP}.lnk"
	Delete "$INSTDIR\oxdrop.exe"
	Delete "$INSTDIR\LICENSE-MIT.txt"
	Delete "$INSTDIR\LICENSE-APACHE.txt"
	Delete "$INSTDIR\uninstall.exe"
	RMDir "$INSTDIR"
	DeleteRegKey HKCU "${UNINSTALL_KEY}"
	DeleteRegKey HKCU "Software\${APP}"
SectionEnd
