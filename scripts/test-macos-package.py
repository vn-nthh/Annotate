#!/usr/bin/env python3
"""Verify the extracted app without opening windows or requesting permissions."""
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile

archive = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='annotate-macos-check-') as temporary:
    directory = Path(temporary)
    subprocess.run(['ditto', '-x', '-k', str(archive), temporary], check=True)
    app = directory/'Annotate.app'
    info = plistlib.loads((app/'Contents/Info.plist').read_bytes())
    assert info['CFBundleIdentifier'] == 'com.annotate.app'
    assert info['NSMicrophoneUsageDescription']
    assert info['LSMinimumSystemVersion'] == '13.0'
    subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    binaries = app/'Contents/MacOS'
    for name in ('annotate', 'whisper-worker-metal'):
        binary = binaries/name
        assert subprocess.check_output(['lipo', '-archs', str(binary)], text=True).strip() == 'arm64'
    result = subprocess.check_output([str(binaries/'annotate'), '--version'], text=True).strip()
    assert result == f"Annotate {info['CFBundleShortVersionString']}"
    worker = subprocess.run([str(binaries/'whisper-worker-metal')], input='{"cmd":"backend"}\n{"cmd":"quit"}\n', capture_output=True, text=True, timeout=15, check=True)
    assert json.loads(worker.stdout.splitlines()[0]) == {'ok': True, 'backend': 'metal'}
    forbidden = {'.exe', '.dll', '.html', '.js', '.otf', '.ttf', '.bin', '.onnx'}
    assert not [p for p in app.rglob('*') if p.is_file() and p.suffix.lower() in forbidden]
    print('Verified arm64 bundle, version, microphone declaration, ad-hoc signature, native CLI startup, and Metal worker protocol. No GUI, microphone, credentials, or keyboard input were used.')
