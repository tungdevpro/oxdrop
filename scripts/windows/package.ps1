# Builds oxdrop.exe for one Windows architecture and packages it in an installer under
# target\package\.
#
#   .\scripts\windows\package.ps1 -Arch x64     # or arm64
#
# Signing is optional. With WINDOWS_CERTIFICATE (a base64-encoded .pfx) and
# WINDOWS_CERTIFICATE_PASSWORD set, the app and the installer are signed and
# timestamped; without them they are left unsigned.
param(
    [Parameter(Mandatory)] [ValidateSet("x64", "arm64")] [string] $Arch
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"$' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$target = @{ x64 = "x86_64-pc-windows-msvc"; arm64 = "aarch64-pc-windows-msvc" }[$Arch]
$out = "target\package"
$installer = "$out\Oxdrop-$version-windows-$Arch-setup.exe"
New-Item -ItemType Directory -Force $out | Out-Null

function Invoke-Checked {
    & $args[0] @($args | Select-Object -Skip 1)
    if ($LASTEXITCODE -ne 0) { throw "$($args[0]) failed with exit code $LASTEXITCODE" }
}

Invoke-Checked cargo build --release --locked -p oxdrop-desktop --target $target
$binary = "target\$target\release\oxdrop.exe"

$certificate = $env:WINDOWS_CERTIFICATE
$pfx = Join-Path ([IO.Path]::GetTempPath()) "oxdrop-signing.pfx"
function Sign-File([string] $path) {
    if (-not $certificate) { return }
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
        Sort-Object FullName -Descending | Select-Object -First 1
    Invoke-Checked $signtool.FullName sign /f $pfx /p $env:WINDOWS_CERTIFICATE_PASSWORD `
        /fd sha256 /tr http://timestamp.digicert.com /td sha256 $path
}
if ($certificate) {
    [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($certificate))
} else {
    Write-Warning "WINDOWS_CERTIFICATE not set: the app and installer are unsigned"
}

try {
    Sign-File $binary

    # Microsoft's small online bootstrapper; it fetches the right runtime for the PC.
    $webview2 = "target\MicrosoftEdgeWebview2Setup.exe"
    if (-not (Test-Path $webview2)) {
        Invoke-WebRequest "https://go.microsoft.com/fwlink/p/?LinkId=2124703" -OutFile $webview2
    }

    $command = Get-Command makensis -ErrorAction SilentlyContinue
    $makensis = if ($command) { $command.Source } else { "${env:ProgramFiles(x86)}\NSIS\makensis.exe" }
    if (-not (Test-Path $makensis)) { throw "makensis not found; install NSIS 3" }
    Invoke-Checked $makensis /V3 /INPUTCHARSET UTF8 `
        "/DVERSION=$version" "/DARCH=$Arch" "/DBINARY=$root\$binary" `
        "/DWEBVIEW2=$root\$webview2" "/DROOT=$root" "/DOUTFILE=$root\$installer" `
        scripts\windows\installer.nsi

    Sign-File $installer
} finally {
    Remove-Item $pfx -ErrorAction SilentlyContinue
}
Write-Output $installer
