#!/bin/sh
# Builds dist/forkz.app from a release build and signs it (see lib.sh for
# which identity is used).
#
#   scripts/bundle.sh
#   SIGN_IDENTITY="..." scripts/bundle.sh   a specific identity ("-" for ad hoc)
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
choose_identity

cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/forkz.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/forkz "$APP/Contents/MacOS/forkz"
cp macos/Info.plist "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
cp macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# Notarization requires the hardened runtime and a timestamp.
sign_with_identity "$APP"
codesign --verify --strict --deep "$APP"
echo "Built $APP ($VERSION), $(codesign -dr - "$APP" 2>&1 | grep designated)"
