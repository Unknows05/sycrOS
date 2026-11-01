#!/bin/bash
# Double-click this file in Finder: opens the sycrOS menu in Terminal.
# Writing to a USB stick needs your password (sudo).
cd "$(dirname "$0")"
xattr -d com.apple.quarantine ./sycros 2>/dev/null   # allow the downloaded program to run
chmod +x ./sycros
echo "sycrOS needs admin rights to write USB sticks. Enter your Mac password:"
sudo ./sycros
