#!/usr/bin/env python3
"""Embed a fresh helper in every Cube build, before signing the app."""
import plistlib
import shutil
import sys
from pathlib import Path

binary, app = map(Path, sys.argv[1:])
contents = app / 'Contents'
helper = contents / 'Helpers/cube.app/Contents'
(helper / 'MacOS').mkdir(parents=True, exist_ok=True)
with (contents / 'Info.plist').open('rb') as f:
    info = plistlib.load(f)
info['CFBundleExecutable'] = 'cube'
info['CFBundleName'] = 'cube'
for key in ('CFBundleIconFile', 'CFBundleDisplayName', 'LSUIElement'):
    info.pop(key, None)
with (helper / 'Info.plist').open('wb') as f:
    plistlib.dump(info, f)
shutil.copy2(binary, helper / 'MacOS/cube')
(helper / 'MacOS/cube').chmod(0o755)
# A normal rebuild is ad-hoc; do not retain provisioning from an earlier build.
for stale in (contents / 'embedded.provisionprofile', helper / 'embedded.provisionprofile'):
    stale.unlink(missing_ok=True)

# Relative symlink for bash/terminal CLI access: Contents/MacOS/cube -> ../Helpers/cube.app/Contents/MacOS/cube
cli_symlink = contents / 'MacOS/cube'
cli_symlink.unlink(missing_ok=True)
cli_symlink.symlink_to('../Helpers/cube.app/Contents/MacOS/cube')
