#!/usr/bin/env bash
# Builds Oxdrop.app as a universal (Apple Silicon + Intel) binary for macOS 12 and
# later, and packages it in a .dmg under target/package/.
#
# Signing is optional. With MACOS_SIGNING_IDENTITY set (a "Developer ID Application"
# identity in the keychain), the app and the .dmg are signed with the hardened runtime;
# with APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD also set, the .dmg is notarized
# and stapled. Without them, the app is only ad-hoc signed.
set -euo pipefail

cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
targets=(aarch64-apple-darwin x86_64-apple-darwin)
out=target/package
app="$out/Oxdrop.app"
dmg="$out/Oxdrop-$version-macos-universal.dmg"

export MACOSX_DEPLOYMENT_TARGET=12.0
for target in "${targets[@]}"; do
	cargo build --release --locked -p oxdrop-desktop --target "$target"
done

rm -rf "$app" "$dmg"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/oxdrop" \
	target/aarch64-apple-darwin/release/oxdrop target/x86_64-apple-darwin/release/oxdrop
sed "s/@VERSION@/$version/g" scripts/macos/Info.plist >"$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"
cp LICENSE-MIT LICENSE-APACHE "$app/Contents/Resources/"

identity=${MACOS_SIGNING_IDENTITY:-}
if [[ -n $identity ]]; then
	codesign --force --options runtime --timestamp --sign "$identity" "$app"
else
	echo "MACOS_SIGNING_IDENTITY not set: the app is ad-hoc signed only" >&2
	codesign --force --sign - "$app"
fi
codesign --verify --strict "$app"

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "Oxdrop" -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$dmg"

if [[ -n $identity ]]; then
	codesign --force --timestamp --sign "$identity" "$dmg"
	if [[ -n ${APPLE_ID:-} && -n ${APPLE_TEAM_ID:-} && -n ${APPLE_APP_PASSWORD:-} ]]; then
		xcrun notarytool submit "$dmg" --wait \
			--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD"
		xcrun stapler staple "$dmg"
	else
		echo "APPLE_ID, APPLE_TEAM_ID or APPLE_APP_PASSWORD not set: not notarized" >&2
	fi
fi

lipo -info "$app/Contents/MacOS/oxdrop"
echo "$dmg"
