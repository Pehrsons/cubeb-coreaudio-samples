#!/bin/sh
# Wrap the vpio-levels binary in a minimal signed .app bundle, so macOS gives it an identity of its
# own.
#
# A bare command line tool's capture is attributed to whatever app hosts the shell -- the same
# reason the microphone permission for this tool came from the terminal rather than from the tool
# itself. Anything the system scopes per app is therefore unreadable from a plain build: the Control
# Center mic mode picker, a per-app TCC grant, a remembered mic mode. Inside a bundle the tool is
# its own responsible process, so those become observable and attributable.
#
# Running the executable inside the bundle is not enough: responsibility is assigned by process
# ancestry, so a shell-launched binary is still attributed to the terminal no matter how it is
# signed. Let launchd start it instead, which needs an absolute path and takes arguments after
# --args. Use --log-file, since stdout goes nowhere useful that way:
#
#     ./make-app.sh release
#     open -a "$PWD/target/release/VpioLevels.app" --args \
#         --device "MacBook Pro Microphone" --scenario native-vpio --log-file /tmp/vpio.log
#
# Running the inner executable directly is still fine when attribution does not matter, and keeps
# output in the terminal:
#
#     ./target/release/VpioLevels.app/Contents/MacOS/vpio-levels --device "MacBook Pro Microphone" --knobs
#
# The signature is ad-hoc, so the identity changes whenever the binary is rebuilt and macOS will ask
# for microphone access again. Approve it, or the capture comes back as digital silence.

set -eu

PROFILE="${1:-release}"
case "$PROFILE" in
    release) BIN="target/release/vpio-levels" ;;
    debug) BIN="target/debug/vpio-levels" ;;
    *)
        echo "usage: $0 [release|debug]" >&2
        exit 2
        ;;
esac

if [ ! -f "$BIN" ]; then
    if [ "$PROFILE" = release ]; then
        echo "$BIN not built. Run: cargo build --release" >&2
    else
        echo "$BIN not built. Run: cargo build" >&2
    fi
    exit 1
fi

APP="$(dirname "$BIN")/VpioLevels.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>org.mozilla.vpio-levels</string>
	<key>CFBundleName</key>
	<string>VpioLevels</string>
	<key>CFBundleExecutable</key>
	<string>vpio-levels</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>0.1</string>
	<key>NSMicrophoneUsageDescription</key>
	<string>Measures microphone input levels.</string>
</dict>
</plist>
PLIST

cp "$BIN" "$APP/Contents/MacOS/vpio-levels"

# Optional entitlements, to test whether an app-level declaration rather than the audio
# configuration is what makes macOS offer the Control Center mic mode picker. Safari declares
# com.apple.security.device.microphone where Chrome and Firefox declare
# com.apple.security.device.audio-input.
#
#     ENTITLEMENTS=microphone ./make-app.sh release    # Safari's spelling
#     ENTITLEMENTS=audio-input ./make-app.sh release   # Chrome and Firefox's spelling
#
# Those are App Sandbox entitlements, and strictly they only mean anything alongside
# com.apple.security.app-sandbox -- but App Sandbox needs a real signing identity to create a
# container, and an ad-hoc signature has no team, so the sandboxed app is killed with SIGTRAP at
# launch. SANDBOX=1 requests it anyway for completeness; the default leaves it off, which still
# embeds the entitlement in case the mic mode logic merely reads it.
ENTITLEMENTS="${ENTITLEMENTS:-none}"
SANDBOX="${SANDBOX:-0}"
case "$ENTITLEMENTS" in
    none) SIGN_ARGS="" ;;
    microphone | audio-input)
        # Outside the bundle: a stray unsigned file inside it makes codesign refuse.
        PLIST_PATH="$(dirname "$APP")/vpio-entitlements.plist"
        cat > "$PLIST_PATH" <<ENTS
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
$([ "$SANDBOX" = 1 ] && printf '\t<key>com.apple.security.app-sandbox</key>\n\t<true/>\n')	<key>com.apple.security.device.$ENTITLEMENTS</key>
	<true/>
</dict>
</plist>
ENTS
        SIGN_ARGS="--entitlements $PLIST_PATH"
        echo "signing with com.apple.security.device.$ENTITLEMENTS$([ "$SANDBOX" = 1 ] && echo ' and App Sandbox')"
        ;;
    *)
        echo "ENTITLEMENTS must be none, microphone or audio-input" >&2
        exit 2
        ;;
esac

# No hardened runtime: it would make dyld drop DYLD_INSERT_LIBRARIES, which the zeroing malloc
# interposer needs.
# shellcheck disable=SC2086
codesign --force --sign - --identifier org.mozilla.vpio-levels $SIGN_ARGS "$APP"

APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
echo "Built $APP"
echo "As its own responsible app, which is what Control Center and TCC attribute to:"
echo "  open -a \"$APP_ABS\" --args --device \"MacBook Pro Microphone\" --scenario native-vpio --log-file /tmp/vpio.log"
echo "Attributed to the terminal, but with output here:"
echo "  $APP/Contents/MacOS/vpio-levels --device \"MacBook Pro Microphone\" --scenario native-vpio"
