# Shared by bundle.sh and make_dmg.sh (sourced, not run).
#
# Signing identity, in order:
#   1. SIGN_IDENTITY, if set ("-" for ad hoc)
#   2. a valid "Developer ID Application" identity in the keychain (of team
#      FORKZ_TEAM_ID, if set): releases are signed this way and notarized
#   3. the self-signed "forkz Release Signing" certificate (local builds
#      without a Developer ID)
#   4. ad hoc
#

RELEASE_IDENTITY="forkz Release Signing"

developer_id() {
    security find-identity -v -p codesigning 2>/dev/null |
        sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' |
        while read -r name; do
            if [ -z "${FORKZ_TEAM_ID:-}" ] || [ "${name%"($FORKZ_TEAM_ID)"}" != "$name" ]; then
                echo "$name"
                break
            fi
        done
}

choose_identity() {
    if [ -n "${SIGN_IDENTITY:-}" ]; then
        return
    fi
    SIGN_IDENTITY=$(developer_id)
    if [ -n "$SIGN_IDENTITY" ]; then
        return
    fi
    if security find-identity -p codesigning | grep -q "\"$RELEASE_IDENTITY\""; then
        SIGN_IDENTITY=$RELEASE_IDENTITY
    else
        echo "warning: no Developer ID or \"$RELEASE_IDENTITY\" identity in the keychain; signing ad hoc" >&2
        SIGN_IDENTITY=-
    fi
}

is_developer_id() {
    case "$SIGN_IDENTITY" in "Developer ID Application:"*) return 0 ;; *) return 1 ;; esac
}

# Notarization needs a Developer ID and an App Store Connect API key:
#   FORKZ_NOTARY_KEY      path to the .p8 key
#   FORKZ_NOTARY_KEY_ID   its key id
#   FORKZ_NOTARY_ISSUER   the issuer id
can_notarize() {
    is_developer_id && [ -n "${FORKZ_NOTARY_KEY:-}" ] && [ -n "${FORKZ_NOTARY_KEY_ID:-}" ] &&
        [ -n "${FORKZ_NOTARY_ISSUER:-}" ]
}

# notarize FILE: submits a .zip or .dmg and waits; prints Apple's log and
# fails unless it is accepted.
notarize() {
    out=$(mktemp)
    xcrun notarytool submit "$1" --key "$FORKZ_NOTARY_KEY" --key-id "$FORKZ_NOTARY_KEY_ID" \
        --issuer "$FORKZ_NOTARY_ISSUER" --wait --output-format json > "$out" || true
    status=$(plutil -extract status raw -o - "$out" 2>/dev/null || echo "submission failed")
    id=$(plutil -extract id raw -o - "$out" 2>/dev/null || true)
    rm -f "$out"
    if [ "$status" != "Accepted" ]; then
        echo "error: notarization of $1: $status" >&2
        if [ -n "$id" ]; then
            xcrun notarytool log "$id" --key "$FORKZ_NOTARY_KEY" --key-id "$FORKZ_NOTARY_KEY_ID" \
                --issuer "$FORKZ_NOTARY_ISSUER" >&2 || true
        fi
        return 1
    fi
    echo "Notarized $1"
}

# staple FILE: attaches the notarization ticket. Right after notarization
# the ticket may not be published yet (stapler exits with 65 or 68), so
# this retries for a while.
staple() {
    for attempt in 1 2 3 4 5 6; do
        xcrun stapler staple -q "$1" && return 0
        echo "stapling $1 failed (attempt $attempt); retrying" >&2
        sleep 15
    done
    xcrun stapler staple "$1"
}

# validate_ticket FILE: checks the stapled ticket, retrying like staple
# (the lookup can fail with 68 until the ticket is published).
validate_ticket() {
    for attempt in 1 2 3 4 5 6; do
        xcrun stapler validate -q "$1" && return 0
        echo "validating the ticket of $1 failed (attempt $attempt); retrying" >&2
        sleep 15
    done
    xcrun stapler validate "$1"
}

# Sparkle (updates), downloaded once into target/sparkle and checked against
# its pinned checksum. Prints the directory.
SPARKLE_VERSION=2.10.0
SPARKLE_SHA256=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c

sparkle_dir() {
    dir="target/sparkle/$SPARKLE_VERSION"
    # Everything used must be there: CI's build cache can restore
    # target/sparkle with some of these files left out.
    if [ ! -d "$dir/Sparkle.framework" ] || [ ! -f "$dir/LICENSE" ] || [ ! -x "$dir/bin/sign_update" ]; then
        mkdir -p target/sparkle
        archive="target/sparkle/Sparkle-$SPARKLE_VERSION.tar.xz"
        curl -sSfL --retry 4 --retry-all-errors --connect-timeout 20 -o "$archive" \
            "https://github.com/sparkle-project/Sparkle/releases/download/$SPARKLE_VERSION/Sparkle-$SPARKLE_VERSION.tar.xz"
        if [ "$(shasum -a 256 "$archive" | cut -d' ' -f1)" != "$SPARKLE_SHA256" ]; then
            rm -f "$archive"
            echo "error: Sparkle $SPARKLE_VERSION does not match its pinned checksum" >&2
            return 1
        fi
        rm -rf "$dir.partial" && mkdir -p "$dir.partial"
        tar -xf "$archive" -C "$dir.partial"
        rm -rf "$dir"
        mv "$dir.partial" "$dir"
    fi
    echo "$dir"
}

# sign_with_identity PATH: the hardened runtime, plus a secure timestamp for
# a Developer ID (Apple's timestamp service only accepts its certificates).
sign_with_identity() {
    if is_developer_id; then
        codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$1"
    else
        codesign --force --options runtime --timestamp=none --sign "$SIGN_IDENTITY" "$1"
    fi
}
