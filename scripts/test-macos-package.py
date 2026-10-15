#!/usr/bin/env python3
"""Verify the extracted app without opening windows or requesting permissions."""
import json
from pathlib import Path
import plistlib
import os
import subprocess
import sys
import tempfile
import time

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
    assert result == info['CFBundleGetInfoString']
    subprocess.run([str(binaries/'annotate'), '--check-platform', str(directory/'profile')], check=True, timeout=20)
    worker = subprocess.run([str(binaries/'whisper-worker-metal')], input='{"cmd":"backend"}\n{"cmd":"quit"}\n', capture_output=True, text=True, timeout=15, check=True)
    assert json.loads(worker.stdout.splitlines()[0]) == {'ok': True, 'backend': 'metal'}
    parent_code = '''
import os, subprocess, sys, time
env = dict(os.environ, ANNOTATE_UI_TEST='1')
worker = subprocess.Popen([sys.argv[1], '--check-parent-lifetime'], stdout=subprocess.PIPE, text=True, env=env)
assert worker.stdout.readline().strip() == 'ready'
print(worker.pid, flush=True)
time.sleep(30)
'''
    parent = subprocess.Popen([sys.executable, '-c', parent_code, str(binaries/'whisper-worker-metal')], stdout=subprocess.PIPE, text=True)
    try:
        worker_pid = int(parent.stdout.readline().strip())
        parent.terminate()
        parent.wait(timeout=5)
        deadline = time.monotonic() + 5
        while True:
            try:
                os.kill(worker_pid, 0)
            except ProcessLookupError:
                break
            assert time.monotonic() < deadline, 'The busy Metal worker survived its parent'
            time.sleep(0.02)
    finally:
        if parent.poll() is None:
            parent.kill()
            parent.wait(timeout=5)
    forbidden = {'.exe', '.dll', '.html', '.js', '.otf', '.ttf', '.bin', '.onnx'}
    assert not [p for p in app.rglob('*') if p.is_file() and p.suffix.lower() in forbidden]
    print('Verified arm64 bundle, version, microphone declaration, ad-hoc signature, native CLI startup, AppKit menu-bar lifecycle, Carbon shortcut registration/removal, Metal worker protocol, and busy-worker termination after parent exit. No GPUI rendering, microphone, credentials, or keyboard input were used.')
