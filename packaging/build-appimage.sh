#!/usr/bin/env bash
# Builds dist/WiFiSpanner-x86_64.AppImage from an existing release build.
# Run from the repository root after `cargo build --release`.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

target="${CARGO_TARGET_DIR:-target}/release"
appdir="dist/AppDir"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" dist

cp "$target/wifispanner" "$appdir/usr/bin/wifispanner"
cp "$target/spike" "$appdir/usr/bin/wifispanner-spike"
cp packaging/wifispanner.desktop "$appdir/wifispanner.desktop"
mkdir -p "$appdir/usr/share/doc/wifispanner"
cp LICENSE NOTICE THIRD-PARTY-LICENSES.txt "$appdir/usr/share/doc/wifispanner/"
cp packaging/AppRun "$appdir/AppRun"
chmod +x "$appdir/AppRun"

# Downscale the 2000px logo for the icon; ImageMagick keeps the alpha channel.
if command -v magick >/dev/null 2>&1; then im=magick; else im=convert; fi
"$im" assets/logo.png -resize 256x256 "$appdir/wifispanner.png"
cp "$appdir/wifispanner.png" "$appdir/.DirIcon"

# A pinned release of appimagetool, checked against its SHA-256 before it is made executable or
# run. To move to a newer release, change both lines to the new release and its published digest.
tool_version="1.9.1"
tool_sha256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
tool="dist/appimagetool-${tool_version}-x86_64.AppImage"
if [ ! -x "$tool" ] || ! echo "$tool_sha256  $tool" | sha256sum -c --status -; then
    rm -f "$tool"
    curl -fsSL -o "$tool.part" \
        "https://github.com/AppImage/appimagetool/releases/download/${tool_version}/appimagetool-x86_64.AppImage"
    echo "$tool_sha256  $tool.part" | sha256sum -c -
    chmod +x "$tool.part"
    mv "$tool.part" "$tool"
fi

# --appimage-extract-and-run avoids needing FUSE on the build machine.
ARCH=x86_64 "$tool" --appimage-extract-and-run "$appdir" dist/WiFiSpanner-x86_64.AppImage
echo "built dist/WiFiSpanner-x86_64.AppImage"
