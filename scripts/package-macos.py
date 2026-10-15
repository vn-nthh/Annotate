#!/usr/bin/env python3
"""Bundle the arm64 build and its non-system dylibs; ad-hoc sign and archive it."""
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
version = re.search(r'^version\s*=\s*"([^"]+)"', (root/'src-tauri/Cargo.toml').read_text(), re.M)[1]
numeric_version = version.split('-', 1)[0]
target = Path(os.environ.get('CARGO_TARGET_DIR', root/'src-tauri/target'))
release = target/'aarch64-apple-darwin/release'
dist = root/'dist'
dist.mkdir(exist_ok=True)

def run(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()

with tempfile.TemporaryDirectory(prefix='annotate-macos-') as temporary:
    stage = Path(temporary)
    app = stage/'Annotate.app'
    binaries = app/'Contents/MacOS'
    resources = app/'Contents/Resources'
    frameworks = app/'Contents/Frameworks'
    for directory in (binaries, resources, frameworks):
        directory.mkdir(parents=True)
    for name in ('annotate', 'whisper-worker-metal'):
        shutil.copy2(release/name, binaries/name)
        assert run('lipo', '-archs', binaries/name) == 'arm64', f'{name} is not arm64'
    shutil.copy2(root/'src-tauri/icons/icon.icns', resources/'icon.icns')
    shutil.copy2(root/'src-tauri/assets/fonts/FFL.txt', resources/'Satoshi-FFL.txt')
    (app/'Contents/Info.plist').write_bytes(plistlib.dumps({
        'CFBundleName': 'Annotate', 'CFBundleDisplayName': 'Annotate',
        'CFBundleIdentifier': 'com.annotate.app', 'CFBundleExecutable': 'annotate',
        'CFBundlePackageType': 'APPL', 'CFBundleIconFile': 'icon.icns',
        'CFBundleShortVersionString': numeric_version, 'CFBundleVersion': numeric_version,
        'CFBundleGetInfoString': f'Annotate {version}',
        'LSMinimumSystemVersion': '13.0', 'NSHighResolutionCapable': True,
        'NSMicrophoneUsageDescription': 'Annotate records audio while you hold your transcription hotkey.',
        'NSPrincipalClass': 'NSApplication',
    }))

    pending = list(binaries.iterdir())
    handled = set()
    while pending:
        binary = pending.pop()
        if binary in handled:
            continue
        handled.add(binary)
        for line in run('otool', '-L', binary).splitlines()[1:]:
            dependency = line.strip().split(' (compatibility version', 1)[0]
            if dependency.startswith(('/System/Library/', '/usr/lib/', '@executable_path/../Frameworks/')):
                continue
            # A dylib's own install ID appears as the first dependency.
            if binary.parent == frameworks and Path(dependency).name == binary.name:
                continue
            source = Path(dependency)
            if not source.is_file():
                source = release/Path(dependency).name
            if not source.is_file():
                raise RuntimeError(f'Unbundled dependency in {binary.name}: {dependency}')
            bundled = frameworks/source.name
            if not bundled.exists():
                shutil.copy2(source, bundled)
                run('install_name_tool', '-id', f'@rpath/{bundled.name}', bundled)
                pending.append(bundled)
            run('install_name_tool', '-change', dependency, f'@executable_path/../Frameworks/{bundled.name}', binary)
    for binary in sorted(handled, key=lambda path: path.parent == binaries):
        run('codesign', '--force', '--sign', '-', '--timestamp=none', binary)
    run('codesign', '--force', '--sign', '-', '--timestamp=none', app)
    run('codesign', '--verify', '--deep', '--strict', app)
    assert run(binaries/'annotate', '--version') == f'Annotate {version}'
    archive = dist/f'Annotate-{version}-macos-arm64.zip'
    run('ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', app, archive)
    (stage/'Applications').symlink_to('/Applications', target_is_directory=True)
    image = dist/f'Annotate-{version}-macos-arm64.dmg'
    run('hdiutil', 'create', '-volname', 'Annotate', '-srcfolder', stage, '-ov', '-format', 'UDZO', image)
    for artifact in (archive, image):
        print(f'{hashlib.sha256(artifact.read_bytes()).hexdigest()}  {artifact.name}')
