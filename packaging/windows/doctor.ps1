# Checks that everything needed to build YQT on Windows is present, and says what is missing.
. "$PSScriptRoot\env.ps1" 2>&1 | Out-Null
$ok = $true
function Check($name, $pass, $hint) {
    if ($pass) { Write-Host "  ok       $name" } else { Write-Host "  MISSING  $name  -> $hint"; $script:ok = $false }
}
function Have($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

$root = if ($env:YTM_QT_ROOT) { $env:YTM_QT_ROOT } else { 'C:\CraftRoot' }
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"

Check 'cargo / rustc'       ((Have cargo) -and (Have rustc)) 'install rustup (just win-setup)'
Check 'MSVC toolchain'      ((Test-Path $vswhere) -and (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)) 'install VS Build Tools with the C++ workload'
Check 'just'                (Have just) 'winget install Casey.Just'
Check "qmake under $root"   ((Test-Path "$root\bin\qmake6.exe") -or (Test-Path "$root\bin\qmake.exe")) 'just win-setup (or set YTM_QT_ROOT)'
Check 'Qt Quick QML module' (Test-Path "$root\qml\QtQuick") 'craft qtdeclarative'
Check 'Qt Multimedia module'(Test-Path "$root\qml\QtMultimedia") 'craft qtmultimedia'
Check 'Kirigami QML module' (Test-Path "$root\qml\org\kde\kirigami") 'craft kirigami'
Check 'KDE desktop style'   (Test-Path "$root\qml\org\kde\desktop") 'craft qqc2-desktop-style'
Check 'windeployqt'         ((Test-Path "$root\bin\windeployqt6.exe") -or (Test-Path "$root\bin\windeployqt.exe")) 'craft qt6-base'

if ($ok) { Write-Host 'All good.' } else { Write-Host 'Something is missing (see above).'; exit 1 }
