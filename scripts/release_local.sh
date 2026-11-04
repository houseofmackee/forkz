#!/bin/sh
# Publishes a release from this Mac, doing what the Release workflow does:
# for when GitHub's macOS runners are unavailable (jobs cancelled with "not
# acquired by Runner").
#
#   scripts/release_local.sh            the version tag at HEAD (vX.Y.Z)
#
# Needs: a clean checkout at a pushed vX.Y.Z tag; the Developer ID in the
# login Keychain; ~/Developer/AppSigning (notary key and ids.env, and the
# forkz Sparkle key); gh logged in as houseofmackee (it is switched there for
# the release and back to MacDozer afterwards, whatever happens); and the
# github-houseofmackee SSH alias for the Homebrew tap.
#
# If the workflow later runs for the same tag, it stops at "release already
# exists" without changing anything.
set -eu
cd "$(dirname "$0")/.."

SIGNING="$HOME/Developer/AppSigning"
TAG=$(git describe --tags --exact-match HEAD 2>/dev/null) || {
    echo "error: HEAD has no version tag (tag it vX.Y.Z and push the tag first)" >&2
    exit 1
}
VERSION=${TAG#v}
if [ -n "$(git status --porcelain)" ]; then
    echo "error: the checkout has uncommitted changes" >&2
    exit 1
fi
if ! git ls-remote --exit-code --tags origin "refs/tags/$TAG" > /dev/null; then
    echo "error: $TAG is not pushed (git push origin $TAG)" >&2
    exit 1
fi
CARGO_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$VERSION" != "$CARGO_VERSION" ]; then
    echo "error: $TAG does not match Cargo.toml version $CARGO_VERSION" >&2
    exit 1
fi
scripts/release_notes.sh "$VERSION" > /dev/null

# Build, notarize, and package, signed like the workflow.
set -a
. "$SIGNING/app-store-connect/ids.env"
set +a
FORKZ_TEAM_ID=$APPLE_TEAM_ID \
FORKZ_NOTARY_KEY=$(ls "$SIGNING"/app-store-connect/AuthKey_*.p8 | head -1) \
FORKZ_NOTARY_KEY_ID=$NOTARY_KEY_ID \
FORKZ_NOTARY_ISSUER=$NOTARY_ISSUER_ID \
FORKZ_SPARKLE_KEY="$SIGNING/sparkle/forkz.key" \
    scripts/dist.sh

# Fail rather than publish anything Gatekeeper would block.
. scripts/lib.sh
DMG="dist/forkz-$VERSION-arm64.dmg"
validate_ticket "$DMG"
spctl --assess --type open --context context:primary-signature "$DMG"
validate_ticket dist/forkz.app
spctl --assess --type execute dist/forkz.app
test -s dist/appcast.xml

# The release, as the latest regular release, with the workflow's notes.
NOTES=$(mktemp)
TAP=$(mktemp -d)
restore() {
    gh auth switch --user MacDozer > /dev/null 2>&1 || true
    rm -rf "$NOTES" "$TAP"
}
trap restore EXIT INT TERM
{
    echo "## What's new"
    echo
    scripts/release_notes.sh "$VERSION" | sed 's/^###/####/'
    echo
    cat <<NOTES
## Installing

Download \`forkz-$VERSION-arm64.dmg\` (or the zip), open it, and drag forkz into Applications. It is signed with a Developer ID and notarized by Apple, so it opens like any other app.

Requires macOS 14 or later on Apple silicon. Checksums are in \`SHA256SUMS.txt\`.

forkz updates itself (forkz > Check for Updates…), or install it with Homebrew: \`brew install --cask houseofmackee/tap/forkz\`.
NOTES
} > "$NOTES"
gh auth switch --user houseofmackee > /dev/null
gh release create "$TAG" -R houseofmackee/forkz --latest --verify-tag --generate-notes --notes-file "$NOTES" \
    --title "forkz $VERSION" \
    "$DMG" "dist/forkz-$VERSION-arm64.zip" dist/SHA256SUMS.txt dist/appcast.xml
gh auth switch --user MacDozer > /dev/null

# The Homebrew cask, from the workflow's own template.
SHA=$(awk '/\.dmg$/ {print $1}' dist/SHA256SUMS.txt)
git clone -q git@github-houseofmackee:houseofmackee/homebrew-tap.git "$TAP/tap"
ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    step = workflow["jobs"]["release"]["steps"].find { |s| s["name"] == "Update the Homebrew cask" }
    File.write(ARGV[1], step["run"][/mkdir -p Casks.*?^CASK\n/m])
' .github/workflows/release.yml "$TAP/cask.sh"
(
    cd "$TAP/tap"
    version=$VERSION sha=$SHA sh "$TAP/cask.sh"
    git add Casks/forkz.rb
    if git diff --cached --quiet; then
        echo "the cask is already at $VERSION"
    else
        git -c user.name="MacKenzie Glanzer" -c user.email="17761324+houseofmackee@users.noreply.github.com" \
            commit -q -m "forkz $VERSION"
        git push -q origin HEAD
        echo "Updated Casks/forkz.rb to $VERSION"
    fi
)
echo "Released forkz $VERSION: https://github.com/houseofmackee/forkz/releases/tag/$TAG"
