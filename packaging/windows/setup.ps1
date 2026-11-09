# One-time setup of a Windows build machine: Rust, Visual Studio Build Tools, just, and KDE Craft with Qt 6 + Kirigami.
# Safe to re-run; each step is skipped when already done. Run from a normal (non-admin) PowerShell.
# Craft builds Qt and the KDE frameworks from source/binary caches, so expect the first run to take a long time and several GB.
param(
    [string]$CraftRoot = $(if ($env:YTM_QT_ROOT) { $env:YTM_QT_ROOT } else { 'C:\CraftRoot' }),
    [string]$CraftTarget = 'windows-cl-msvc2022-x86_64'
)
$ErrorActionPreference = 'Stop'

function Have($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

Write-Host '== winget tools'
if (-not (Have winget)) { throw 'winget is missing; install "App Installer" from the Microsoft Store.' }
if (-not (Have rustup)) { winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements }
if (-not (Have just))   { winget install --id Casey.Just -e --accept-source-agreements --accept-package-agreements }
if (-not (Have git))    { winget install --id Git.Git -e --accept-source-agreements --accept-package-agreements }
if (-not (Have python)) { winget install --id Python.Python.3.12 -e --accept-source-agreements --accept-package-agreements }

Write-Host '== MSVC build tools'
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$hasMsvc = (Test-Path $vswhere) -and (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
if (-not $hasMsvc) {
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements `
        --override '--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
}

Write-Host '== Rust toolchain'
if (Have rustup) { rustup default stable-x86_64-pc-windows-msvc }

Write-Host "== KDE Craft in $CraftRoot ($CraftTarget)"
if (-not (Test-Path "$CraftRoot\craft\craftenv.ps1")) {
    # Official bootstrap: https://community.kde.org/Craft#Windows
    $boot = Join-Path $env:TEMP 'craft-bootstrap.ps1'
    Invoke-WebRequest 'https://raw.githubusercontent.com/KDE/craft/master/setup/install_craft.ps1' -OutFile $boot
    & $boot -root $CraftRoot -prefix $CraftRoot -targets $CraftTarget
}
# Qt (with Quick, QML, Multimedia), Kirigami, the desktop Qt Quick Controls style and icons.
& "$CraftRoot\craft\craftenv.ps1" -Command "craft qt6-base qtdeclarative qtmultimedia qtsvg kirigami qqc2-desktop-style breeze-icons"

Write-Host ''
Write-Host "Done. Next: 'just win-doctor' to verify, then 'just win-build'."
Write-Host "If Craft used a different root, set YTM_QT_ROOT=$CraftRoot in your environment."
