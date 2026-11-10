#!/usr/bin/env bash
set -euo pipefail

target=${1:?Expected a Rust target}
output=${2:?Expected an output path}
mode=${3:-native}
if [[ $(uname -s) != Linux ]]; then
    echo "AppImage packaging requires Linux. On macOS, use script/build_release_local.sh." >&2
    exit 1
fi
case "$mode" in
    native|container) ;;
    *) echo "Unsupported AppImage packaging mode: $mode" >&2; exit 1 ;;
esac
arch=${target%%-*}
case "$arch" in
    x86_64) tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
    aarch64) tool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
    *) echo "Unsupported AppImage target: $target" >&2; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/ViSpace.AppDir"
mkdir -p "$app/usr/bin" "$app/usr/lib" "$app/usr/share/licenses/vispace"
bin_dir="${CARGO_TARGET_DIR:-target}/$target/release"
install -m755 "$bin_dir/vispace" "$bin_dir/vispace-discovery" "$app/usr/bin/"
install -m644 LICENSE THIRD_PARTY_NOTICES.md assets/licenses/*.txt "$app/usr/share/licenses/vispace/"
install -m644 packaging/vispace.desktop "$app/vispace.desktop"
install -m644 assets/app-icon.png "$app/vispace.png"
ln -s vispace.png "$app/.DirIcon"

# Keep the host's graphics, display and C runtime libraries. Bundle the
# D-Bus client library linked by the Bluetooth backend.
dbus=$(ldd "$app/usr/bin/vispace" | awk '$1 == "libdbus-1.so.3" { print $3 }')
if [[ -n "$dbus" ]]; then
    install -m644 "$dbus" "$app/usr/lib/libdbus-1.so.3"
fi
cat > "$app/AppRun" <<'ENTRYPOINT'
#!/bin/sh
set -eu
app_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PATH="$app_dir/usr/bin:$PATH"
export LD_LIBRARY_PATH="$app_dir/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$app_dir/usr/bin/vispace" "$@"
ENTRYPOINT
chmod +x "$app/AppRun"

curl --fail --location --retry 3 -o "$work/runtime" \
    "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$arch"
if [[ "$mode" == container ]]; then
    # Build and inspect the same Type 2 layout without executing its ELF wrapper.
    mksquashfs "$app" "$work/payload.squashfs" -noappend -all-root -comp zstd
    image="$work/ViSpace.AppImage"
    cat "$work/runtime" "$work/payload.squashfs" > "$image"
    offset=$(stat -c '%s' "$work/runtime")
    unsquashfs -no-progress -offset "$offset" -d "$work/extracted" "$image"
    "$work/extracted/AppRun" --check --data "$work/smoke.sqlite"
    install -m755 "$image" "$output"
else
    curl --fail --location --retry 3 -o "$work/appimagetool" \
        "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage"
    printf '%s  %s\n' "$tool_sha" "$work/appimagetool" | sha256sum --check
    chmod +x "$work/appimagetool"
    ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$work/appimagetool" --runtime-file "$work/runtime" "$app" "$output"
    APPIMAGE_EXTRACT_AND_RUN=1 "$output" --check --data "$work/smoke.sqlite"
fi
