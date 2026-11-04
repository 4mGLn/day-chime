#!/bin/sh
# Day Chime user-local installer. Downloaded archives are verified before extraction.
set -eu
command -v python3 >/dev/null 2>&1 || { echo 'Day Chime requires Python 3.11 or newer.' >&2; exit 1; }
exec python3 - "$@" <<'PY'
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit

if sys.version_info < (3, 11):
    raise SystemExit('Day Chime requires Python 3.11 or newer.')
import tomllib

REPOSITORY = '4mGLn/day-chime'
HOST = 'com.day_chime.attendance'
VERSION = re.compile(r'v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?')


def fail(message):
    raise RuntimeError(message)


def absolute(value):
    path = Path(value).expanduser().absolute()
    if any(ord(char) < 32 for char in str(path)):
        fail('Paths must not contain control characters.')
    return path


def canonical_url(value):
    url = urlsplit(value)
    if (url.scheme != 'https' or not url.hostname or
            not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.daouoffice\.com', url.hostname) or
            url.path != '/home' or url.query or url.fragment or url.username or url.password or url.netloc.lower() != url.hostname):
        fail('--url must be https://YOUR-COMPANY.daouoffice.com/home, without credentials or query parameters.')
    return f'https://{url.hostname}/home'


def normalized_version(value):
    if not VERSION.fullmatch(value or ''):
        fail('Invalid release version; use v0.4.0 or another published semantic version.')
    return value if value.startswith('v') else 'v' + value


def fetch(url, output=None):
    if not shutil.which('curl'):
        fail('curl is required to download a release.')
    args = ['curl', '--fail', '--silent', '--show-error', '--location', '--proto', '=https', '--proto-redir', '=https', '--tlsv1.2', '--connect-timeout', '15', '--max-time', '300', '--retry', '2', url]
    if output:
        subprocess.run(args + ['--output', str(output)], check=True)
        return None
    return subprocess.check_output(args, text=True)


def verify_checksum(archive, checksums):
    expected = None
    for line in checksums.read_text().splitlines():
        match = re.fullmatch(r'([a-fA-F0-9]{64}) [ *](.+)', line)
        if match and match.group(2) == archive.name:
            if expected is not None:
                fail('Duplicate checksum entry for release archive.')
            expected = match.group(1).lower()
    if expected is None:
        fail('No matching checksum entry for release archive.')
    digest = hashlib.sha256()
    with archive.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != expected:
        fail('Release checksum verification failed; nothing was installed.')


def extract_safely(archive, directory):
    with tarfile.open(archive, 'r:gz') as bundle:
        entries = bundle.getmembers()
        seen = set()
        total = 0
        for entry in entries:
            path = PurePosixPath(entry.name)
            if (path.is_absolute() or '..' in path.parts or '\\' in entry.name or
                    not path.parts or entry.name in seen or not (entry.isdir() or entry.isfile())):
                fail('Unsafe or duplicate archive entry: ' + entry.name)
            seen.add(entry.name)
            total += entry.size
            if entry.size > 512 * 1024 * 1024 or total > 1024 * 1024 * 1024:
                fail('Release archive is unexpectedly large.')
        for entry in entries:
            target = directory.joinpath(*PurePosixPath(entry.name).parts)
            if entry.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with bundle.extractfile(entry) as source, target.open('wb') as destination:
                    shutil.copyfileobj(source, destination)
                target.chmod(0o755 if entry.mode & 0o111 else 0o644)


def atomic_file(source, destination, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix='.' + destination.name + '-', dir=destination.parent)
    try:
        with os.fdopen(descriptor, 'wb') as output, source.open('rb') as input_file:
            shutil.copyfileobj(input_file, output)
        os.chmod(temporary, mode)
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def write_file(destination, text, mode=0o600):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix='.' + destination.name + '-', dir=destination.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            output.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def copy_directory(source, destination, overlay=False, prepare=None):
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix='.' + destination.name + '-', dir=destination.parent))
    backup = temporary.with_name(temporary.name + '-previous')
    try:
        if overlay and destination.exists():
            shutil.copytree(destination, temporary, dirs_exist_ok=True)
        shutil.copytree(source, temporary, dirs_exist_ok=True)
        if prepare:
            prepare(temporary)
        if destination.exists():
            destination.rename(backup)
        temporary.rename(destination)
        if backup.exists():
            shutil.rmtree(backup)
    except BaseException:
        if backup.exists() and not destination.exists():
            backup.rename(destination)
        raise
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)


def extension_manifest(directory):
    manifest_path = directory / 'manifest.json'
    if manifest_path.is_symlink() or not manifest_path.is_file():
        fail('Extension directory needs an existing Day Chime manifest.json.')
    manifest = json.loads(manifest_path.read_text())
    if (manifest.get('manifest_version') != 3 or not isinstance(manifest.get('name'), str)
            or not re.fullmatch(r'Day Chime(?: [—–-] .+)?', manifest['name'])):
        fail('Extension directory is not a recognized Day Chime extension.')
    if not isinstance(manifest.get('content_scripts'), list):
        fail('Day Chime extension manifest has no content scripts.')
    for content in manifest['content_scripts']:
        if not isinstance(content, dict):
            fail('Invalid Day Chime content script definition.')
    if 'key' in manifest and not isinstance(manifest['key'], str):
        fail('Invalid extension public key.')
    return manifest


def validate_extension_target(directory, canonical, app, config_root, prefix):
    # Resolve .. only after refusing symlink components, including a dangling leaf.
    for component in (directory, *directory.parents):
        if component.is_symlink():
            fail('Extension directory must not contain symlink components.')
    directory = directory.resolve()
    canonical = canonical.resolve()
    if directory != canonical:
        reserved = [app.resolve(), config_root.resolve(), (prefix / 'bin').resolve()]
        for protected in reserved:
            if directory == protected or directory in protected.parents or protected in directory.parents:
                fail('Extension directory overlaps an application, configuration or executable directory.')
        # Never replace HOME, the prefix itself, or any of their ancestors.
        for protected in [Path.home().resolve(), prefix.resolve()]:
            if directory == protected or directory in protected.parents:
                fail('Extension directory must be a dedicated extension folder.')
    if directory.exists():
        if not directory.is_dir():
            fail('Extension directory is not a directory.')
        if any(entry.is_symlink() for entry in directory.rglob('*')):
            fail('Extension directory must not contain symlink entries.')
        extension_manifest(directory)
    return directory


def deploy_extension(source, destination, url):
    incoming = extension_manifest(source)
    previous = extension_manifest(destination) if destination.exists() else {}
    def configure(staged):
        manifest = dict(incoming)
        if previous.get('key'):
            manifest['key'] = previous['key']
        for content in manifest['content_scripts']:
            content['matches'] = [url + '*']
        write_file(staged / 'site.js', 'globalThis.DayChimeSite = ' + json.dumps({'sourceUrl': url}) + ';\n', 0o644)
        write_file(staged / 'manifest.json', json.dumps(manifest, indent=2) + '\n', 0o644)
    # Keep development files, tests and other unmanaged files in an unpacked tree.
    copy_directory(source, destination, overlay=True, prepare=configure)


def unit_quote(value, executable=False):
    value = str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
    if executable:
        value = value.replace('$', '$$')
    return '"' + value + '"'


def main():
    parser = argparse.ArgumentParser(prog='install.sh', description='Install Day Chime prebuilt Linux binaries and configure the browser bridge without sudo.')
    parser.add_argument('--url', help='Company home URL, required for first installation')
    parser.add_argument('--browser', choices=['brave', 'chrome', 'chromium'])
    parser.add_argument('--version', help='Pin a release, for example v0.4.0 (default: latest release)')
    parser.add_argument('--prefix', default=str(Path.home() / '.local'), help='Executable prefix (default: ~/.local)')
    parser.add_argument('--extension-dir', help='Existing unpacked extension directory to update (remembered for future updates)')
    parser.add_argument('--extension-id', help='ID shown after loading the unpacked browser extension')
    parser.add_argument('--configure-only', action='store_true', help='Configure an existing installation without downloading')
    parser.add_argument('--no-downgrade', action='store_true', help='Refuse replacing a newer installed version (used by the updater)')
    parser.add_argument('--preserve-service', action='store_true', help='Keep an existing user service unit unchanged')
    parser.add_argument('--no-start', action='store_true', help='Install/configure without starting or enabling the user service')
    parser.add_argument('--archive', type=Path, help='Use a local release archive (requires --checksum-file)')
    parser.add_argument('--checksum-file', type=Path, help='SHA256SUMS for --archive')
    args = parser.parse_args()
    if platform.system() != 'Linux' or platform.machine().lower() not in ('x86_64', 'amd64'):
        fail('Prebuilt installation currently supports Linux x86_64 only. See the repository for source builds on other platforms.')
    if bool(args.archive) != bool(args.checksum_file):
        fail('--archive and --checksum-file must be used together.')
    if args.configure_only and args.archive:
        fail('--configure-only cannot be combined with --archive.')
    if args.extension_id and not re.fullmatch(r'[a-p]{32}', args.extension_id):
        fail('--extension-id must contain exactly 32 lowercase letters a-p.')
    requested_version = normalized_version(args.version) if args.version else None
    prefix = absolute(args.prefix)
    config_root = absolute(os.environ.get('XDG_CONFIG_HOME', str(Path.home() / '.config')))
    data_root = absolute(os.environ.get('XDG_DATA_HOME', str(Path.home() / '.local/share')))
    app = data_root / 'day-chime'
    config_path = config_root / 'day-chime/config.toml'
    config_before = config_path.read_bytes() if config_path.exists() else None
    old_config = tomllib.loads(config_before.decode()) if config_before is not None else None
    url = canonical_url(args.url or (old_config or {}).get('browser_url', ''))
    if old_config is None and not args.url:
        fail('--url is required for the first installation.')
    metadata_path = app / 'install-state.json'
    metadata_before = metadata_path.read_bytes() if metadata_path.exists() else None
    metadata = json.loads(metadata_before) if metadata_before is not None else {}
    browser = args.browser or metadata.get('browser', 'brave')
    if browser not in ('brave', 'chrome', 'chromium'):
        fail('Stored browser is invalid. Pass a supported --browser.')
    extension_id = args.extension_id or metadata.get('extension_id')
    if extension_id and not re.fullmatch(r'[a-p]{32}', extension_id):
        fail('Stored extension ID is invalid. Pass a valid --extension-id.')
    canonical_extension = app / 'extension'
    extension = absolute(args.extension_dir or metadata.get('extension_dir') or canonical_extension)
    # Validate all destinations before binaries, configuration or metadata can change.
    extension = validate_extension_target(extension, canonical_extension, app, config_root, prefix)
    canonical_extension = validate_extension_target(canonical_extension, canonical_extension, app, config_root, prefix)
    config_contents = None
    if old_config is None:
        config_contents = 'source = "browser"\nbrowser_url = ' + json.dumps(url) + '\ntimezone = "Asia/Seoul"\nwork_minutes = 480\nbreak_minutes = 60\npoll_seconds = 300\n'
    elif args.url:
        updated = dict(old_config)
        updated['browser_url'] = url
        def toml_value(value):
            if isinstance(value, str):
                return json.dumps(value, ensure_ascii=False)
            if isinstance(value, bool):
                return 'true' if value else 'false'
            if isinstance(value, (int, float)):
                return repr(value)
            if isinstance(value, list):
                return '[' + ', '.join(toml_value(item) for item in value) + ']'
            fail('Cannot rewrite a non-flat Day Chime configuration; edit browser_url manually.')
        config_contents = '\n'.join(json.dumps(key) + ' = ' + toml_value(value) for key, value in updated.items()) + '\n'
        tomllib.loads(config_contents)
    app.mkdir(parents=True, exist_ok=True)
    lock_flags = os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0)
    installation_lock = os.fdopen(os.open(app / 'installer.lock', lock_flags, 0o600), 'a+')
    try:
        fcntl.flock(installation_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        fail('Another Day Chime installer is running; retry after it finishes.')
    def verify_unchanged_settings():
        for path, before in [(config_path, config_before), (metadata_path, metadata_before)]:
            current = path.read_bytes() if path.exists() else None
            if current != before:
                fail('Configuration or installation metadata changed during installation; retry using the current settings.')
        # The download may have taken time; refuse a newly introduced unsafe target
        # before the first binary or configuration is replaced.
        validate_extension_target(extension, canonical_extension, app, config_root, prefix)
        validate_extension_target(canonical_extension, canonical_extension, app, config_root, prefix)
    if args.configure_only:
        required = [prefix / 'bin/day-chime', prefix / 'bin/day-chime-browser-host', app / 'extension/manifest.json']
        if not all(path.is_file() for path in required):
            fail('Existing installation not found; run the normal installer first.')
        version = metadata.get('version', 'unknown')
        extension_manifest(canonical_extension)
        verify_unchanged_settings()
        deploy_extension(canonical_extension, canonical_extension, url)
        if extension != canonical_extension:
            deploy_extension(canonical_extension, extension, url)
    else:
        with tempfile.TemporaryDirectory(prefix='day-chime-install-') as work:
            work = Path(work)
            if args.archive:
                archive = absolute(args.archive)
                checksums = absolute(args.checksum_file)
            else:
                version = requested_version or normalized_version(json.loads(fetch(f'https://api.github.com/repos/{REPOSITORY}/releases/latest'))['tag_name'])
                name = f'day-chime-{version}-linux-x86_64.tar.gz'
                archive, checksums = work / name, work / 'SHA256SUMS'
                base = f'https://github.com/{REPOSITORY}/releases/download/{version}'
                fetch(base + '/SHA256SUMS', checksums)
                fetch(base + '/' + name, archive)
            verify_checksum(archive, checksums)
            extracted = work / 'extracted'
            extracted.mkdir()
            extract_safely(archive, extracted)
            release = json.loads((extracted / 'release.json').read_text())
            version = normalized_version(release['version'])
            if requested_version and version != requested_version:
                fail('Archive version does not match --version.')
            if release.get('platform') != 'linux-x86_64':
                fail('Archive platform does not match this installer.')
            for relative in ('bin/day-chime', 'bin/day-chime-browser-host', 'extension/manifest.json', 'extension/site.js', 'config.example.toml', 'install.sh', 'LICENSE'):
                if not (extracted / relative).is_file():
                    fail('Release is missing required file: ' + relative)
            extension_manifest(extracted / 'extension')
            verify_unchanged_settings()
            if args.no_downgrade:
                def version_numbers(value):
                    match = re.fullmatch(r'v?(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?', value or '')
                    return tuple(map(int, match.groups())) if match else None
                installed_versions = []
                current_binary = prefix / 'bin/day-chime'
                if current_binary.is_file():
                    result = subprocess.run([str(current_binary), '--version'], capture_output=True, text=True, timeout=10, check=False)
                    match = re.search(r'(?:^|\s)(v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?=\s|$)', result.stdout)
                    if result.returncode != 0 or not match:
                        fail('Cannot establish installed binary version safely; refusing an unattended update.')
                    installed_versions.append(version_numbers(match.group(1)))
                # Reread while holding the shared installer lock: another installer
                # may have upgraded between initial option parsing and this attempt.
                if metadata_path.exists():
                    installed = version_numbers(json.loads(metadata_path.read_text()).get('version'))
                    if installed is not None:
                        installed_versions.append(installed)
                if installed_versions and max(installed_versions) > version_numbers(version):
                    fail('A newer Day Chime version is already installed; refusing to downgrade.')
                if current_binary.exists() and not installed_versions:
                    fail('Cannot establish installed version safely; update without --no-downgrade only after checking it manually.')
            for name in ('day-chime', 'day-chime-browser-host'):
                atomic_file(extracted / 'bin' / name, prefix / 'bin' / name, 0o755)
            deploy_extension(extracted / 'extension', canonical_extension, url)
            if extension != canonical_extension:
                deploy_extension(extracted / 'extension', extension, url)
            for name in ('assets', 'deploy'):
                if (extracted / name).is_dir():
                    copy_directory(extracted / name, app / name)
            for name in ('install.sh', 'config.example.toml', 'LICENSE'):
                atomic_file(extracted / name, app / name, 0o755 if name == 'install.sh' else 0o644)
    if config_contents is not None:
        write_file(config_path, config_contents)
    for size in (16, 32, 48, 64, 128, 256, 512):
        source = app / 'assets' / f'day-chime-{size}.png'
        if source.is_file():
            atomic_file(source, data_root / 'icons/hicolor' / f'{size}x{size}' / 'apps/day-chime.png')
    if shutil.which('gtk-update-icon-cache'):
        subprocess.run(['gtk-update-icon-cache', '-f', '-t', str(data_root / 'icons/hicolor')], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    launcher = app / 'native-host-launcher'
    write_file(launcher, '#!/bin/sh\nexport XDG_CONFIG_HOME=' + shlex.quote(str(config_root)) + '\nexport XDG_DATA_HOME=' + shlex.quote(str(data_root)) + '\nexec ' + shlex.quote(str(prefix / 'bin/day-chime-browser-host')) + ' "$@"\n', 0o755)
    if extension_id:
        browser_folder = {'brave': 'BraveSoftware/Brave-Browser', 'chrome': 'google-chrome', 'chromium': 'chromium'}[browser]
        host = {'name': HOST, 'description': 'Day Chime attendance bridge', 'path': str(launcher), 'type': 'stdio', 'allowed_origins': [f'chrome-extension://{extension_id}/']}
        write_file(config_root / browser_folder / 'NativeMessagingHosts' / f'{HOST}.json', json.dumps(host, indent=2) + '\n')
    service = '\n'.join(['[Unit]', 'Description=Day Chime attendance reminder', 'After=graphical-session.target', 'PartOf=graphical-session.target', '', '[Service]', 'Type=simple', 'ExecStart=' + unit_quote(prefix / 'bin/day-chime', True) + ' --config ' + unit_quote(config_path, True) + ' run', 'Environment=' + unit_quote('XDG_CONFIG_HOME=' + str(config_root)), 'Environment=' + unit_quote('XDG_DATA_HOME=' + str(data_root)), 'Restart=on-failure', 'RestartSec=30', 'UMask=0077', '', '[Install]', 'WantedBy=graphical-session.target', ''])
    service_path = config_root / 'systemd/user/day-chime.service'
    if not args.preserve_service or not service_path.exists():
        write_file(service_path, service, 0o644)
    write_file(metadata_path, json.dumps({'version': version, 'browser': browser, 'extension_id': extension_id, 'prefix': str(prefix), 'extension_dir': str(extension), 'canonical_extension_dir': str(canonical_extension)}, indent=2) + '\n')
    setup = '#!/bin/sh\nexec /bin/sh ' + shlex.quote(str(app / 'install.sh')) + ' --configure-only --prefix ' + shlex.quote(str(prefix)) + ' "$@"\n'
    write_file(prefix / 'bin/day-chime-setup', setup, 0o755)
    print(f'Day Chime {version} installed for {url}')
    print(f'Extension directory: {extension}')
    print('Reload Day Chime once in your browser extension manager to activate updated extension files.')
    print(f'Configuration: {config_path}')
    if extension_id and not args.no_start:
        if not shutil.which('systemctl'):
            fail('systemctl is unavailable. Files are installed; run day-chime manually or rerun with --no-start.')
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        subprocess.run(['systemctl', '--user', 'enable', '--now', 'day-chime.service'], check=True)
        # A binary may have been replaced during an update of an already-running daemon.
        subprocess.run(['systemctl', '--user', 'restart', 'day-chime.service'], check=True)
        print('User service enabled and running. Use day-chime check to inspect the saved attendance.')
    elif args.no_start:
        print('Service start skipped (--no-start).')
    if not extension_id:
        print('Load the extension directory above in your browser, then copy its 32-letter ID.')
        print('Finish setup: ' + shlex.quote(str(prefix / 'bin/day-chime-setup')) + ' --extension-id YOUR_EXTENSION_ID')
        print('The service will be enabled after this setup step. Browser login, if needed, is manual.')
    print('No browser passwords, cookies or attendance records were uploaded.')


try:
    main()
except (RuntimeError, OSError, ValueError, KeyError, subprocess.CalledProcessError, subprocess.TimeoutExpired, tarfile.TarError) as error:
    print('Day Chime installer: ' + str(error), file=sys.stderr)
    raise SystemExit(1)
PY
