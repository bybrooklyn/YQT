#!/usr/bin/env bash
# A fake `qmake -query` for cross-compiling to Windows from Linux: cxx-qt-build asks qmake where Qt is.
# Headers/libs/plugins come from the Windows kit; moc, rcc and friends come from the matching Linux (host) kit.
# Env: YTM_CROSS_QT (default <repo>/.cross/qt), YTM_CROSS_QT_VERSION (default 6.10.3).
set -euo pipefail
root="${YTM_CROSS_QT:-$(cd "$(dirname "$0")/../.." && pwd)/.cross/qt}"
ver="${YTM_CROSS_QT_VERSION:-6.10.3}"
win="$root/$ver/msvc2022_64"
host="$root/$ver/gcc_64"

# Host tools, seen through a directory of our own so rcc can be told not to use zstd: the Windows Qt does not
# export qResourceFeatureZstd, so resources compressed with zstd fail to link.
tools="$root/host-tools"
if [ ! -x "$tools/rcc" ]; then
    mkdir -p "$tools"
    for f in "$host"/libexec/* "$host"/bin/*; do [ -f "$f" ] && ln -sf "$f" "$tools/$(basename "$f")"; done
    rm -f "$tools/rcc"
    printf '#!/bin/sh\nexec "%s" --no-zstd "$@"\n' "$host/libexec/rcc" > "$tools/rcc"
    chmod +x "$tools/rcc"
fi

[ "${1:-}" = "-query" ] || { echo "qmake-shim: only -query is supported" >&2; exit 1; }
case "${2:-}" in
    QT_VERSION)                              echo "$ver" ;;
    QT_INSTALL_PREFIX|QT_INSTALL_ARCHDATA)   echo "$win" ;;
    QT_INSTALL_HEADERS)                      echo "$win/include" ;;
    QT_INSTALL_LIBS)                         echo "$win/lib" ;;
    QT_INSTALL_PLUGINS)                      echo "$win/plugins" ;;
    QT_INSTALL_QML)                          echo "$win/qml" ;;
    QT_INSTALL_BINS)                         echo "$win/bin" ;;
    QT_INSTALL_LIBEXECS)                     echo "$tools" ;;
    QT_HOST_PREFIX)                          echo "$host" ;;
    QT_HOST_BINS|QT_HOST_BINS/get)           echo "$tools" ;;
    QT_HOST_LIBEXECS|QT_HOST_LIBEXECS/get)   echo "$tools" ;;
    QT_HOST_LIBS)                            echo "$host/lib" ;;
    *)                                       echo "" ;;
esac
