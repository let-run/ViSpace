#!/usr/bin/env python3
"""Register the discovery-only helper for one installed Chrome extension, for this user."""
import argparse
import json
import platform
import re
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('extension_id', help='ID shown for the unpacked extension at chrome://extensions')
parser.add_argument('--helper', type=Path, help='Path to built vispace-discovery executable')
args = parser.parse_args()
if not re.fullmatch(r'[a-p]{32}', args.extension_id):
    parser.error('Expected a 32-character Chrome extension ID.')
system = platform.system()
name = 'run.let.vispace_discovery'
helper = (args.helper or Path(__file__).resolve().parents[1] / 'target' / 'debug' /
          ('vispace-discovery.exe' if system == 'Windows' else 'vispace-discovery')).resolve()
if not helper.is_file():
    parser.error('Build the helper first with cargo build --bin vispace-discovery, or supply --helper.')
if system == 'Darwin':
    directory = Path.home() / 'Library/Application Support/Google/Chrome/NativeMessagingHosts'
elif system == 'Linux':
    import os
    directory = Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config')) / 'google-chrome/NativeMessagingHosts'
elif system == 'Windows':
    import os
    directory = Path(os.environ['LOCALAPPDATA']) / 'VInventory/NativeMessagingHosts'
else:
    parser.error('Unsupported operating system.')
directory.mkdir(parents=True, exist_ok=True)
manifest = directory / (name + '.json')
manifest.write_text(json.dumps({'name': name, 'description': 'VInventory nearby sync discovery',
                               'path': str(helper), 'type': 'stdio',
                               'allowed_origins': ['chrome-extension://' + args.extension_id + '/']}, indent=2))
if system == 'Windows':
    import winreg
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, r'Software\Google\Chrome\NativeMessagingHosts' + chr(92) + name) as key:
        winreg.SetValueEx(key, '', 0, winreg.REG_SZ, str(manifest))
print(f'Registered {manifest}')
