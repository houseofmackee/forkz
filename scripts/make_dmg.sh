#!/bin/sh
# Builds dist/forkz-<version>-arm64.dmg for direct download (Apple silicon
# only), with a SHA-256 checksum next to it, then mounts it to check it.
#
#   scripts/make_dmg.sh                 builds forkz.app first (scripts/bundle.sh)
#   SKIP_BUNDLE=1 scripts/make_dmg.sh   packages the existing dist/forkz.app
#
# Signed with a Developer ID and given the App Store Connect API key
# (FORKZ_NOTARY_KEY, FORKZ_NOTARY_KEY_ID, FORKZ_NOTARY_ISSUER; see lib.sh),
# the app and the DMG are notarized and stapled, so they open like any other
# app. Otherwise the DMG includes a note on opening an unnotarized app.
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
choose_identity

if [ -z "${SKIP_BUNDLE:-}" ]; then
    scripts/bundle.sh
fi
APP=dist/forkz.app
[ -d "$APP" ] || { echo "error: $APP not found (run scripts/bundle.sh)" >&2; exit 1; }

ARCHS=$(lipo -archs "$APP/Contents/MacOS/forkz")
if [ "$ARCHS" != "arm64" ]; then
    echo "error: expected an arm64-only binary, found: $ARCHS" >&2
    exit 1
fi
codesign --verify --strict "$APP"
if codesign -dv "$APP" 2>&1 | grep -q "Signature=adhoc"; then
    echo "warning: $APP is signed ad hoc; Sparkle updates and Keychain access will not carry over" >&2
fi

VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
DMG="dist/forkz-$VERSION-arm64.dmg"
WORK=$(mktemp -d)
trap 'hdiutil detach -quiet "$WORK/mnt" 2>/dev/null || true; rm -rf "$WORK"' EXIT

NOTARIZED=""
if can_notarize; then
    # The app first, stapled, so the DMG (and dist.sh's zip) carry the ticket.
    ditto -c -k --sequesterRsrc --keepParent "$APP" "$WORK/forkz.zip"
    notarize "$WORK/forkz.zip"
    staple "$APP"
    NOTARIZED=1
elif is_developer_id; then
    echo "warning: signed with a Developer ID but not notarized (FORKZ_NOTARY_KEY, _KEY_ID and _ISSUER are not set)" >&2
fi

STAGE="$WORK/forkz"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/forkz.app"
ln -s /Applications "$STAGE/Applications"
if [ -n "$NOTARIZED" ]; then
    cat > "$STAGE/Read Me First.txt" <<EOF
forkz $VERSION for Apple silicon Macs (macOS 14 Sonoma or later)

Install: drag forkz into Applications, then open it from there.

forkz is a dual-pane file manager with SFTP. Connect to a server with
Command-K, or import your sites with File > Import from FileZilla.

To browse folders such as Desktop, Documents, and external drives without
being asked each time, allow forkz in System Settings > Privacy & Security >
Full Disk Access.

Source code and checksums: see the release page you downloaded this from.
EOF
else
    cat > "$STAGE/Read Me First.txt" <<EOF
forkz $VERSION for Apple silicon Macs (macOS 14 Sonoma or later)

Install: drag forkz into Applications, then open it from there.

This build is not notarized by Apple, so the first time you open it macOS
says it cannot check it. To open it anyway:

  1. Open forkz once and click Done in the warning.
  2. Go to System Settings > Privacy & Security, scroll down and click
     Open Anyway next to the message about forkz.
  3. Confirm with Open.

If Open Anyway does not appear, run this in Terminal instead and then
open forkz again:

  xattr -dr com.apple.quarantine /Applications/forkz.app

forkz is a dual-pane file manager with SFTP. Connect to a server with
Command-K.

Source code and checksums: see the release page you downloaded this from.
EOF
fi

rm -f "$DMG" "$DMG.sha256"
hdiutil create -quiet -volname "forkz $VERSION" -srcfolder "$STAGE" -fs HFS+ -format ULMO -ov "$DMG"
if is_developer_id; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
fi
if [ -n "$NOTARIZED" ]; then
    notarize "$DMG"
    staple "$DMG"
fi

# Check what a user will get: the image is intact and the app inside is
# complete, signed and Apple silicon.
# hdiutil can fail with "resource busy" right after creating an image, and
# -quiet hides why; retry, and run the last attempt without -quiet.
retry() {
    for attempt in 1 2; do
        hdiutil "$@" -quiet 2>/dev/null && return 0
        sleep 2
    done
    hdiutil "$@" || { echo "error: hdiutil $1 failed for $DMG" >&2; exit 1; }
}
retry verify "$DMG"
mkdir -p "$WORK/mnt"
retry attach -nobrowse -readonly -mountpoint "$WORK/mnt" "$DMG"
codesign --verify --strict "$WORK/mnt/forkz.app"
[ "$(lipo -archs "$WORK/mnt/forkz.app/Contents/MacOS/forkz")" = "arm64" ]
[ -L "$WORK/mnt/Applications" ]
if [ -n "$NOTARIZED" ]; then
    # What Gatekeeper decides for a downloaded copy.
    spctl --assess --type execute "$WORK/mnt/forkz.app"
    validate_ticket "$WORK/mnt/forkz.app"
fi
hdiutil detach -quiet "$WORK/mnt"
if [ -n "$NOTARIZED" ]; then
    spctl --assess --type open --context context:primary-signature "$DMG"
    validate_ticket "$DMG"
fi

(cd dist && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256")
echo "Built $DMG ($(du -h "$DMG" | cut -f1 | tr -d ' '))"
cat "$DMG.sha256"
