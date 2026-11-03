#!/bin/sh
# Build a self-contained, checksummed Linux x86_64 release from existing binaries.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 - "$root" "$@" <<'PY'
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

root = Path(sys.argv[1])
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, default=root / 'dist')
args = parser.parse_args(sys.argv[2:])
version = 'v' + tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']
if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version):
    raise SystemExit('Invalid Cargo release version')
binary = args.bin_dir / 'day-chime'
if subprocess.check_output([str(binary.resolve()), '--version'], text=True, timeout=30).strip() != 'day-chime ' + version[1:]:
    raise SystemExit('Built binary version does not match Cargo.toml')
args.output.mkdir(parents=True, exist_ok=True)
archive = args.output / f'day-chime-{version}-linux-x86_64.tar.gz'
with tempfile.TemporaryDirectory(prefix='day-chime-package-') as work:
    stage = Path(work)
    for name in ('day-chime', 'day-chime-browser-host'):
        source = args.bin_dir / name
        if not source.is_file(): raise SystemExit(f'Missing binary: {source}')
        target = stage / 'bin' / name
        target.parent.mkdir(exist_ok=True)
        shutil.copy2(source, target)
        target.chmod(0o755)
    for name in ('extension', 'assets', 'deploy'):
        shutil.copytree(root / name, stage / name, ignore=shutil.ignore_patterns('__pycache__', '*.pyc', 'tests'))
    for name in ('config.example.toml', 'LICENSE', 'README.md', 'install.sh'):
        shutil.copy2(root / name, stage / name)
    (stage / 'release.json').write_text(json.dumps({'version': version, 'platform': 'linux-x86_64'}, indent=2) + '\n')
    with tarfile.open(archive, 'w:gz') as output:
        for path in sorted(stage.rglob('*')):
            output.add(path, arcname=path.relative_to(stage), recursive=False)
checksums = args.output / 'SHA256SUMS'
checksums.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
print(archive)
print(checksums)
PY
