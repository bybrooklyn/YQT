#!/usr/bin/env bash
# Bundle the release build with the Qt, Kirigami and Breeze files it needs into dist/yqt-linux/ and zip it.
# Unzip anywhere and run ./yqt.sh. System pieces that must match the machine (libc, GPU, X11/Wayland, fontconfig,
# dbus) are not bundled. Usage: packaging/linux/bundle.sh [path-to-yqt-binary]
set -euo pipefail
cd "$(dirname "$0")/../.."
bin="${1:-target/release/yqt}"
[ -x "$bin" ] || { echo "no $bin: run 'cargo build --release' first" >&2; exit 1; }
qtplug="$(qmake6 -query QT_INSTALL_PLUGINS)"
qtqml="$(qmake6 -query QT_INSTALL_QML)"
out=dist/yqt-linux
rm -rf "$out" dist/yqt-linux-x86_64.zip
mkdir -p "$out/lib" "$out/plugins" "$out/qml" "$out/share/icons/hicolor/scalable/apps"
cp "$bin" "$out/yqt"

# --- QML modules the app imports (and what they import) --------------------------------------------------------
for m in Qt QtCore QtQml QtQuick QtMultimedia org/kde/kirigami org/kde/desktop org/kde/qqc2desktopstyle \
         org/kde/iconthemes org/kde/kquickcontrolsaddons; do
    [ -d "$qtqml/$m" ] && { mkdir -p "$out/qml/$(dirname "$m")"; cp -a "$qtqml/$m" "$out/qml/$(dirname "$m")/"; }
done
# --- Qt plugins -------------------------------------------------------------------------------------------------
for p in platforms styles imageformats iconengines tls networkinformation multimedia \
         xcbglintegrations wayland-decoration-client wayland-graphics-integration-client wayland-shell-integration \
         platforminputcontexts; do
    [ -d "$qtplug/$p" ] && cp -a "$qtplug/$p" "$out/plugins/"
done

# --- shared libraries: the closure of everything copied, minus what must come from the host -------------------
skip='^(linux-vdso|ld-linux|libc\.|libm\.|libdl\.|libpthread|librt\.|libutil|libresolv|libnsl|libGL|libEGL|libGLX|libGLdispatch|libOpenGL|libvulkan|libdrm|libgbm|libX|libxcb|libxkbcommon|libwayland|libfontconfig|libfreetype|libharfbuzz|libdbus-1|libgcc_s|libstdc\+\+|libasound|libpulse|libudev|libsystemd|libselinux|libz\.|libglib|libgobject|libgmodule|libgio|libffi|libpcre|libmount|libblkid|libcap)'
mapfile -t objs < <(find "$out" -type f \( -name '*.so' -o -name '*.so.*' \) ; echo "$out/yqt")
for o in "${objs[@]}"; do
    ldd "$o" 2>/dev/null | awk '/=> \//{n=$1; sub(/.*\//,"",n); print n, $3}' || true
done | sort -u | while read -r name path; do
    [[ "$name" =~ $skip ]] && continue
    [ -e "$out/lib/$name" ] || cp -L "$path" "$out/lib/$name"
done
# second pass for libraries pulled in by the libraries just copied
find "$out/lib" -type f -name '*.so*' | while read -r o; do
    ldd "$o" 2>/dev/null | awk '/=> \//{n=$1; sub(/.*\//,"",n); print n, $3}'
done | sort -u | while read -r name path; do
    [[ "$name" =~ $skip ]] && continue
    [ -e "$out/lib/$name" ] || cp -L "$path" "$out/lib/$name"
done

# --- icons, launcher -------------------------------------------------------------------------------------------
cp assets/logo.svg "$out/share/icons/hicolor/scalable/apps/yqt.svg"
cp LICENSE "$out/"
if [ "${YQT_BUNDLE_ICONS:-1}" = 1 ] && [ -d /usr/share/icons/breeze ]; then cp -a /usr/share/icons/breeze "$out/share/icons/"; fi
cat > "$out/yqt.sh" <<'L'
#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="$here/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export QML_IMPORT_PATH="$here/qml"
export QML2_IMPORT_PATH="$here/qml"
export QT_PLUGIN_PATH="$here/plugins"
export XDG_DATA_DIRS="$here/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "$here/yqt" "$@"
L
chmod +x "$out/yqt.sh" "$out/yqt"
(cd dist && zip -qr yqt-linux-x86_64.zip yqt-linux)
echo "wrote dist/yqt-linux-x86_64.zip ($(du -h dist/yqt-linux-x86_64.zip | cut -f1))"
