#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == --help ]]; then
    echo "Usage: $0 <Developer ID Application identity hash>"
    echo "List local identities with: security find-identity -v -p codesigning"
    exit 0
fi
if [[ $# != 1 || $(uname -s) != Darwin || ! "$1" =~ ^[[:xdigit:]]{40}$ ]]; then
    echo "Run on macOS with one Developer ID Application identity hash. See --help." >&2
    exit 2
fi

identity=$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]')
if ! security find-identity -v -p codesigning |
    awk -v hash="$identity" '$2 == hash && /"Developer ID Application:/ { found = 1 } END { exit !found }'; then
    echo "No valid Developer ID Application identity with that hash in your Keychain." >&2
    exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for tool in cargo rustup cargo-xwin docker python3; do
    command -v "$tool" >/dev/null || { echo "Missing required tool: $tool" >&2; exit 1; }
done
xcrun --sdk macosx --show-sdk-path >/dev/null
docker info >/dev/null

windows_path=$PATH
if command -v brew >/dev/null; then
    windows_path="$(brew --prefix llvm)/bin:$(brew --prefix lld)/bin:$windows_path"
fi
for tool in clang clang-cl lld-link llvm-lib; do
    PATH="$windows_path" command -v "$tool" >/dev/null || {
        echo "Missing $tool. Install LLVM and LLD or add them to PATH." >&2
        exit 1
    }
done

version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c '
import json, sys
print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "vispace"))
')
export VISPACE_PACKAGE_VERSION="$version"
export CARGO_TARGET_DIR="$root/target/local-release"
mkdir -p "$root/dist" "$CARGO_TARGET_DIR"
output=$(mktemp -d "$root/dist/local-release.XXXXXX")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf 'Release output: %s\n' "$output"

mac_targets=(aarch64-apple-darwin x86_64-apple-darwin)
windows_targets=(x86_64-pc-windows-msvc aarch64-pc-windows-msvc)
rustup target add "${mac_targets[@]}" "${windows_targets[@]}"
for target in "${mac_targets[@]}"; do
    echo "Building $target"
    MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --locked --release --bins --target "$target"
    stage="$work/$target"
    app="$stage/ViSpace.app"
    VISPACE_APP_BUNDLE="$app" ./script/build_and_run.sh --package-only "$CARGO_TARGET_DIR/$target/release"
    arch=${target%%-*}
    mach_arch=$arch
    if [[ "$arch" == aarch64 ]]; then mach_arch=arm64; fi
    lipo "$app/Contents/MacOS/vispace" -verify_arch "$mach_arch"
    lipo "$app/Contents/MacOS/vispace-discovery" -verify_arch "$mach_arch"
    codesign --force --timestamp --options runtime --sign "$identity" "$app/Contents/MacOS/vispace-discovery"
    codesign --force --timestamp --options runtime --sign "$identity" "$app"
    codesign --verify --deep --strict "$app"
    if [[ $(uname -m) == "$mach_arch" ]]; then
        "$app/Contents/MacOS/vispace" --check --data "$work/mac-smoke.sqlite"
    fi
    ln -s /Applications "$stage/Applications"
    dmg="ViSpace-$version-macos-$arch.dmg"
    hdiutil makehybrid -hfs -hfs-volume-name ViSpace -o "$work/uncompressed-$arch.dmg" "$stage"
    hdiutil convert "$work/uncompressed-$arch.dmg" -format UDZO -o "$work/$dmg"
    codesign --force --timestamp --sign "$identity" "$work/$dmg"
    codesign --verify "$work/$dmg"
    hdiutil verify "$work/$dmg"
    mv "$work/$dmg" "$output/"
done

for target in "${windows_targets[@]}"; do
    echo "Building $target"
    compiler=clang-cl
    # ring uses the clang driver on Windows ARM64, so its SDK flags must match.
    if [[ "$target" == aarch64-pc-windows-msvc ]]; then compiler=clang; fi
    PATH="$windows_path" cargo xwin build --cross-compiler "$compiler" --locked --release --bins --target "$target"
    mkdir -p "$output/$target"
    cp "$CARGO_TARGET_DIR/$target/release/"{vispace,vispace-discovery}.exe "$output/$target/"
done

for arch in x86_64 aarch64; do
    platform=linux/amd64
    if [[ "$arch" == aarch64 ]]; then platform=linux/arm64; fi
    target="$arch-unknown-linux-gnu"
    image="vispace-local-linux:$arch"
    cache="$CARGO_TARGET_DIR/docker-$arch"
    mkdir -p "$cache"
    echo "Building and packaging $target in Docker"
    docker build --platform "$platform" --tag "$image" - <<'DOCKERFILE'
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential clang libclang-dev pkg-config \
    libdbus-1-dev libudev-dev libxkbcommon-dev libwayland-dev squashfs-tools \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal --default-toolchain stable \
    && rm /tmp/rustup-init.sh
ENV PATH=/opt/cargo/bin:${PATH}
DOCKERFILE
    docker run --rm --platform "$platform" --user "$(id -u):$(id -g)" \
        -e HOME=/cache -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
        --volume "$root:/workspace:ro" --volume "$cache:/cache" --volume "$output:/output" \
        --workdir /workspace "$image" bash -euo pipefail -c '
            cargo build --locked --release --bins --target "$1"
            ./script/package_appimage.sh "$1" "/output/$2" container
        ' bash "$target" "ViSpace-$version-linux-$arch.AppImage"
done

printf 'Finished. Release artifacts: %s\n' "$output"
