# Bundles target\release\yqt.exe with Qt, the QML modules it imports (incl. Kirigami) and icons into dist\YQT-windows\,
# then zips it. Run after `just win-build` (the justfile recipe does both).
param([string]$Out = 'dist\YQT-windows')
. "$PSScriptRoot\env.ps1"
$ErrorActionPreference = 'Stop'

$exe = 'target\release\yqt.exe'
if (-not (Test-Path $exe)) { throw "$exe not found; run 'just win-build' first." }

if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force $Out | Out-Null
Copy-Item $exe $Out

$deploy = Join-Path $env:YTM_QT_ROOT 'bin\windeployqt6.exe'
if (-not (Test-Path $deploy)) { $deploy = Join-Path $env:YTM_QT_ROOT 'bin\windeployqt.exe' }
# --qmldir scans qml\ for imports; --qmlimport lets it find Kirigami and the desktop style in Craft's qml dir.
& $deploy --release --no-translations --qmldir qml --qmlimport $env:QML_IMPORT_PATH "$Out\yqt.exe"
if ($LASTEXITCODE -ne 0) { throw "windeployqt failed ($LASTEXITCODE)" }

# windeployqt only follows Qt's own modules; copy the KDE ones it may have skipped, plus Breeze icons.
foreach ($m in 'org\kde\kirigami', 'org\kde\desktop', 'org\kde\sonnet', 'org\kde\iconthemes') {
    $src = Join-Path $env:QML_IMPORT_PATH $m
    if (Test-Path $src) {
        $dst = Join-Path $Out "qml\$m"
        if (-not (Test-Path $dst)) { New-Item -ItemType Directory -Force (Split-Path $dst) | Out-Null; Copy-Item -Recurse $src $dst }
    }
}
Get-ChildItem (Join-Path $env:YTM_QT_ROOT 'bin') -Filter 'KF6*.dll' -ErrorAction SilentlyContinue | Copy-Item -Destination $Out -Force
# Kirigami finds the Breeze icon theme as data\icons\breeze\breeze-icons.rcc next to the exe.
$icons = Join-Path $env:YTM_QT_ROOT 'bin\data\icons\breeze'
if (Test-Path $icons) { New-Item -ItemType Directory -Force "$Out\data\icons" | Out-Null; Copy-Item -Recurse -Force $icons "$Out\data\icons\" }
else { Write-Warning 'Breeze icons not found (craft breeze-icons); the app will have no icons.' }

Compress-Archive -Path "$Out\*" -DestinationPath "$Out.zip" -Force
Write-Host "Packaged: $Out  and  $Out.zip"
