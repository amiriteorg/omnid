#!/usr/bin/env bash
# Builds libomnid_android.so for the app with the NDK's clang.
#
#   android/build-rust.sh [debug|release]   (default: release)
#
# Finds the SDK from ANDROID_HOME, ANDROID_SDK_ROOT or the macOS default and
# uses its newest NDK unless ANDROID_NDK_HOME is set.
set -euo pipefail

profile=${1:-release}
root=$(cd "$(dirname "$0")/.." && pwd)
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}
ndk=${ANDROID_NDK_HOME:-$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1)}
[ -d "$ndk" ] || { echo "no NDK found; install one with sdkmanager 'ndk;<version>'" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) host=darwin-x86_64 ;;  # also the directory name on Apple silicon
  Linux) host=linux-x86_64 ;;
  *) echo "unsupported build host $(uname -s)" >&2; exit 1 ;;
esac
bin=$ndk/toolchains/llvm/prebuilt/$host/bin
api=29  # minSdk
target=aarch64-linux-android

export CC_aarch64_linux_android=$bin/$target$api-clang
export CXX_aarch64_linux_android=$bin/$target$api-clang++
export AR_aarch64_linux_android=$bin/llvm-ar
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$bin/$target$api-clang
export ANDROID_NDK_ROOT=$ndk

flags=(--target "$target" -p omnid-android)
[ "$profile" = release ] && flags+=(--release)
cargo build --manifest-path "$root/Cargo.toml" "${flags[@]}"

out=$root/android/app/src/main/jniLibs/arm64-v8a
mkdir -p "$out"
cp "$root/target/$target/$profile/libomnid_android.so" "$out/"
echo "built $out/libomnid_android.so ($(du -h "$out/libomnid_android.so" | cut -f1))"
