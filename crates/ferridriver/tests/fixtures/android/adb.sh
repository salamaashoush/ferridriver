#!/bin/sh
if [ ! -f "$0.called" ]; then
  printf first > "$0.called"
  printf 'No activity found\n'
else
  printf 'priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=true\ncom.android.chrome/com.google.android.apps.chrome.Main\n'
fi
