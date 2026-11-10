#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PROFILE=debug
RUN=1
BIN_DIR=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --release) PROFILE=release ;;
    --build-only) RUN=0 ;;
    --package-only)
      shift
      BIN_DIR=${1:?--package-only requires a binary directory}
      RUN=0
      ;;
    *) printf 'Usage: %s [--release] [--build-only] [--package-only BIN_DIR]\n' "$0" >&2; exit 2 ;;
  esac
  shift
done
if [ -z "$BIN_DIR" ]; then
  (
    cd "$ROOT"
    if [ "$PROFILE" = release ]; then
      cargo build --locked --release
    else
      cargo build --locked
    fi
  )
  BIN_DIR="$ROOT/target/$PROFILE"
elif [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'Packaging a macOS app requires macOS.' >&2
  exit 1
fi
if [ "$(uname -s)" = Darwin ]; then
  APP="$ROOT/dist/ViSpace.app"
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
  # Replace the executable atomically so macOS does not retain a stale code-signature cache.
  cp "$BIN_DIR/vispace" "$APP/Contents/MacOS/vispace.new"
  mv -f "$APP/Contents/MacOS/vispace.new" "$APP/Contents/MacOS/vispace"
  cp "$BIN_DIR/vispace-discovery" "$APP/Contents/MacOS/vispace-discovery"
  # Use a distinct resource name so macOS refreshes the former app artwork.
  cp "$ROOT/assets/AppIcon.icns" "$APP/Contents/Resources/ViSpaceIcon.icns"
  mkdir -p "$APP/Contents/Resources/licenses"
  cp "$ROOT"/assets/licenses/*.txt "$APP/Contents/Resources/licenses/"
  cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>vispace</string>
<key>CFBundleIdentifier</key><string>com.local.BOMInventoryRust</string>
<key>CFBundleName</key><string>ViSpace</string>
<key>CFBundleDisplayName</key><string>ViSpace</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleIconFile</key><string>ViSpaceIcon.icns</string>
<key>NSBluetoothAlwaysUsageDescription</key><string>Find and connect to your Niimbot B1 label printer.</string>
<key>NSLocalNetworkUsageDescription</key><string>Find nearby inventory sync peers.</string>
<key>NSBonjourServices</key><array><string>_vispace._tcp</string></array>
<key>NSCameraUsageDescription</key><string>Scan part QR codes and barcodes to search your inventory.</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSMinimumSystemVersion</key><string>12.0</string>
</dict></plist>
PLIST
  if [ -n "${VISPACE_PACKAGE_VERSION:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VISPACE_PACKAGE_VERSION" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VISPACE_PACKAGE_VERSION" "$APP/Contents/Info.plist"
  fi
  # Seal the development bundle after changing its binary, metadata, and icon.
  codesign --force --sign - "$APP/Contents/MacOS/vispace-discovery"
  codesign --force --sign - "$APP"
  codesign --verify --deep --strict "$APP"
  touch "$APP"
  if [ "$RUN" = 1 ]; then open "$APP"; fi
  printf '%s\n' "$APP"
elif [ "$RUN" = 1 ]; then
  exec "$BIN_DIR/vispace"
fi
