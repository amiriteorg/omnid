#!/usr/bin/env bash
# Installs the app on a headless Android emulator and checks that it works:
# the daemon listens, Tor bootstraps, the search page shows with the name, the
# notification follows the charger, and nothing crashed. Creates and boots the
# emulator when needed, a Pixel 7 with a data partition big enough for the
# app (GeckoView unpacks to about 300 MB).
#
#   scripts/android-smoke.sh [--fresh] [APK]   default APK: the release build
#   scripts/android-smoke.sh stop              shut the emulator down
#
# A screenshot of the search page at the end goes to $TMPDIR/omnid-smoke.png.
# --fresh uninstalls first (a new index, the first-run seeds). Needs the
# Android SDK (ANDROID_HOME) with platform-tools, the emulator and the system
# image below, on a machine that runs arm64 emulators (Apple silicon).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SDK=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}
AVD=omnid-smoke
IMAGE="system-images;android-36;google_apis;arm64-v8a"
PORT=5580
SERIAL=emulator-$PORT
PACKAGE=cc.wckd.omnid
LOG=${TMPDIR:-/tmp}/omnid-smoke-emulator.log
SHOT=${TMPDIR:-/tmp}/omnid-smoke.png

say() { printf '%s\n' "smoke: $*" >&2; }
die() { say "$*"; exit 1; }
adb() { "$SDK/platform-tools/adb" -s "$SERIAL" "$@"; }

wait_for() { # SECONDS DESCRIPTION COMMAND...
  local deadline=$((SECONDS + $1)) what=$2
  shift 2
  until "$@" >/dev/null 2>&1; do
    [ $SECONDS -lt $deadline ] || return 1
    sleep 2
  done
  say "$what"
}

avdmanager() {
  local tool
  # The SDK's own command-line tools, the PATH, or Homebrew's.
  for tool in "$SDK/cmdline-tools/latest/bin/avdmanager" "$(command -v avdmanager || true)" \
    /opt/homebrew/share/android-commandlinetools/cmdline-tools/latest/bin/avdmanager; do
    [ -x "$tool" ] && { ANDROID_SDK_ROOT=$SDK ANDROID_HOME=$SDK "$tool" "$@"; return; }
  done
  die "avdmanager not found; install the Android command-line tools"
}

ensure_avd() {
  [ -d "$HOME/.android/avd/$AVD.avd" ] && return
  [ -d "$SDK/system-images/android-36/google_apis/arm64-v8a" ] ||
    die "missing $IMAGE; install it with sdkmanager \"$IMAGE\" \"emulator\""
  say "creating the $AVD emulator"
  # A phone's screen (1080x2400, 411dp wide), not the 320dp default.
  echo no | avdmanager create avd -n "$AVD" -k "$IMAGE" -d pixel_7 >/dev/null
  printf '%s\n' disk.dataPartition.size=16G hw.ramSize=4096 hw.keyboard=yes >>"$HOME/.android/avd/$AVD.avd/config.ini"
}

booted() { [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; }

ensure_booted() {
  booted && return
  ensure_avd
  say "booting $AVD (headless, log in $LOG)"
  nohup "$SDK/emulator/emulator" -avd "$AVD" -port "$PORT" -no-window -no-audio -no-boot-anim \
    -gpu swiftshader_indirect -no-snapshot </dev/null >"$LOG" 2>&1 &
  wait_for 300 "booted" booted || die "the emulator did not boot in 5 minutes; see $LOG"
}

case "${1:-}" in
  stop)
    adb emu kill >/dev/null 2>&1 && say "stopped" || say "not running"
    exit 0
    ;;
esac

fresh=false
[ "${1:-}" = --fresh ] && { fresh=true; shift; }
apk=${1:-$ROOT/android/app/build/outputs/apk/release/app-release.apk}
[ -f "$apk" ] || die "no APK at $apk; build it with android/build-rust.sh and ./gradlew assembleRelease"

ensure_booted
if $fresh; then adb uninstall "$PACKAGE" >/dev/null 2>&1 || true; fi
say "installing $(basename "$apk")"
adb install -r "$apk" >/dev/null
adb shell pm grant "$PACKAGE" android.permission.POST_NOTIFICATIONS 2>/dev/null || true
adb shell dumpsys battery reset >/dev/null
adb logcat -c
adb shell am start -n "$PACKAGE/.MainActivity" >/dev/null

failures=0
check() { # DESCRIPTION COMMAND...
  local what=$1
  shift
  if "$@" >/dev/null 2>&1; then say "ok    $what"; else say "FAIL  $what"; failures=$((failures + 1)); fi
}
logged() { adb logcat -d -s omnid | grep -q "$1"; }
screen_has() { adb shell uiautomator dump /sdcard/omnid-ui.xml >/dev/null && adb shell cat /sdcard/omnid-ui.xml | grep -q "$1"; }
notification_says() { adb shell dumpsys notification --noredact | grep -A60 "pkg=$PACKAGE" | grep -q "$1"; }
never_crashed() { ! adb logcat -d | grep -A2 "FATAL EXCEPTION" | grep -q "Process: $PACKAGE"; }

wait_for 60 "the daemon is listening" logged "listening on" || true
check "the daemon started" logged "listening on"
wait_for 180 "Tor bootstrapped" logged "tor: bootstrapped" || true
check "Tor bootstrapped (needs network)" logged "tor: bootstrapped"
# The field's placeholder is not in the dump; the add section always is.
wait_for 30 "the search page is up" screen_has "Add an address" || true
check "the search page shows" screen_has "Add an address"
check "the toolbar says OMNId" screen_has 'text="OMNId"'
adb shell dumpsys battery unplug >/dev/null
wait_for 20 "unplugged" notification_says "waiting for a charger" || true
check "the notification says crawling waits for a charger" notification_says "waiting for a charger"
adb shell dumpsys battery reset >/dev/null
wait_for 20 "plugged back in" notification_says "follows your charger" || true
check "the notification goes back once charging" notification_says "follows your charger"
check "nothing crashed" never_crashed
adb exec-out screencap -p >"$SHOT" && say "screenshot: $SHOT"

if [ $failures -eq 0 ]; then say "PASS"; else say "FAIL ($failures)"; exit 1; fi
