# Dot-source or run via with-env.ps1: points Cargo/cxx-qt at the Qt + Kirigami that KDE Craft installed.
# Override with YTM_QT_ROOT (the Craft root, default C:\CraftRoot) if you installed Craft elsewhere.
# Set YTM_QT_ROOT to a plain Qt kit (e.g. C:\Qt\6.8.0\msvc2022_64) only if Kirigami's QML module is installed into it too.
$ErrorActionPreference = 'Stop'

$root = if ($env:YTM_QT_ROOT) { $env:YTM_QT_ROOT } else { 'C:\CraftRoot' }
$qmake = Join-Path $root 'bin\qmake6.exe'
if (-not (Test-Path $qmake)) { $qmake = Join-Path $root 'bin\qmake.exe' }
if (-not (Test-Path $qmake)) {
    throw "No qmake under $root\bin. Run 'just win-setup' first, or set YTM_QT_ROOT to your Qt/Craft root."
}

$env:YTM_QT_ROOT = $root
$env:QMAKE = $qmake                                   # cxx-qt-build asks this qmake where Qt lives
$env:PATH = "$(Join-Path $root 'bin');$env:PATH"      # Qt DLLs, windeployqt
$env:QML_IMPORT_PATH = Join-Path $root 'qml'          # Kirigami & friends, for the build and for running from target\
$env:QML2_IMPORT_PATH = $env:QML_IMPORT_PATH
