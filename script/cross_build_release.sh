#!/usr/bin/env bash
set -euo pipefail

output=${1:?Pass an absolute artifact output directory}
[[ "$output" = /* ]] || { echo "Expected an absolute artifact output directory" >&2; exit 1; }
[[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ]] || {
    echo "The release build requires an x86_64 Linux host" >&2
    exit 1
}
: "${SDKROOT:?Set SDKROOT on the runner to its macOS SDK directory}"
[[ -d "$SDKROOT/System/Library/Frameworks" ]] || {
    echo "SDKROOT does not contain macOS system frameworks: $SDKROOT" >&2
    exit 1
}
for tool in cargo rustup clang clang-cl lld-link llvm-lib zig cargo-zigbuild cargo-xwin aarch64-linux-gnu-gcc aarch64-linux-gnu-ar pkg-config; do
    command -v "$tool" >/dev/null || { echo "Missing release build tool: $tool" >&2; exit 1; }
done
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$PWD/target"

targets=(
    x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
    x86_64-pc-windows-msvc aarch64-pc-windows-msvc
    x86_64-apple-darwin aarch64-apple-darwin
)
rustup target add "${targets[@]}"
for target in "${targets[@]}"; do
    echo "::group::Compile $target"
    flags=(--locked --release --bins --target "$target")
    suffix=
    case "$target" in
        x86_64-unknown-linux-gnu)
            cargo build "${flags[@]}"
            ;;
        aarch64-unknown-linux-gnu)
            CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
            AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
            PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
            cargo build "${flags[@]}"
            ;;
        *-pc-windows-msvc)
            cargo xwin build "${flags[@]}"
            suffix=.exe
            ;;
        *-apple-darwin)
            MACOSX_DEPLOYMENT_TARGET=12.0 cargo zigbuild "${flags[@]}"
            ;;
    esac
    install -Dm755 "target/$target/release/vispace$suffix" "$output/$target/release/vispace$suffix"
    install -Dm755 "target/$target/release/vispace-discovery$suffix" "$output/$target/release/vispace-discovery$suffix"
    echo "::endgroup::"
done
