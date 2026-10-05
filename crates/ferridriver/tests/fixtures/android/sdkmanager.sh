#!/bin/sh
read -r answer || exit 0
[ "$answer" = y ] || exit 0
touch "$ANDROID_SDK_ROOT/accepted"
printf 'call\n' >> "$ANDROID_SDK_ROOT/calls"
shift
for package in "$@"; do
  case "$package" in
    platform-tools) mkdir -p "$ANDROID_SDK_ROOT/platform-tools"; touch "$ANDROID_SDK_ROOT/platform-tools/adb" ;;
    emulator) mkdir -p "$ANDROID_SDK_ROOT/emulator"; touch "$ANDROID_SDK_ROOT/emulator/emulator" ;;
    system-images*) path=$(printf '%s' "$package" | tr ';' '/'); mkdir -p "$ANDROID_SDK_ROOT/$path"; touch "$ANDROID_SDK_ROOT/$path/package.xml" ;;
    *) exit 42 ;;
  esac
done
