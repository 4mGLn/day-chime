"""Disposable installer contract tests: never use the real user's installation."""
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / 'install.sh'
URL = 'https://example.daouoffice.com/home'
ID = 'a' * 32


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='day-chime-install-test-')
        self.root = Path(self.directory.name)
        self.home = self.root / 'home with spaces'
        self.home.mkdir()
        self.prefix = self.home / 'local prefix'
        self.config = self.home / 'configuration'
        self.data = self.home / 'application data'
        self.env = dict(os.environ, HOME=str(self.home), XDG_CONFIG_HOME=str(self.config), XDG_DATA_HOME=str(self.data))
        self.archive = self.root / 'day-chime-v0.4.0-linux-x86_64.tar.gz'
        self.checksum = self.root / 'SHA256SUMS'
        self.make_archive()

    def tearDown(self):
        self.directory.cleanup()

    def make_archive(self, extra=None, overrides=None):
        entries = {
            'bin/day-chime': b'#!/bin/sh\nprintf "day-chime 0.4.0\\n"\n',
            'bin/day-chime-browser-host': b'#!/bin/sh\nprintf "%s\\n" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"\n',
            'extension/manifest.json': json.dumps({'manifest_version': 3, 'name': 'Day Chime — 근태 읽기', 'content_scripts': [{'matches': [URL + '*'], 'js': ['site.js', 'reader.js', 'content.js']}]}).encode(),
            'extension/content.js': b'// release content v1\n',
            'extension/reader.js': b'// release reader v1\n',
            'extension/popup.js': b'// release popup v1\n',
            'extension/site.js': b'globalThis.DayChimeSite = {sourceUrl: "https://example.daouoffice.com/home"};\n',
            'assets/day-chime-128.png': b'icon-fixture',
            'deploy/systemd/day-chime.service': b'fixture',
            'config.example.toml': b'source="browser"\n',
            'LICENSE': b'MIT fixture',
            'README.md': b'fixture readme',
            'install.sh': INSTALLER.read_bytes(),
            'release.json': json.dumps({'version': 'v0.4.0', 'platform': 'linux-x86_64'}).encode(),
        }
        entries.update(overrides or {})
        with tarfile.open(self.archive, 'w:gz') as output:
            for name, data in entries.items():
                item = tarfile.TarInfo(name)
                item.size = len(data)
                item.mode = 0o755 if name.startswith('bin/') or name == 'install.sh' else 0o644
                output.addfile(item, io.BytesIO(data))
            if extra:
                output.addfile(*extra)
        self.checksum.write_text(hashlib.sha256(self.archive.read_bytes()).hexdigest() + '  ' + self.archive.name + '\n')

    def run_installer(self, *arguments, configure=False, success=True):
        args = ['sh', str(INSTALLER), '--prefix', str(self.prefix)]
        if configure:
            args += ['--configure-only']
        else:
            args += ['--archive', str(self.archive), '--checksum-file', str(self.checksum)]
        result = subprocess.run(args + list(arguments), env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_install_with_spaces_generates_narrow_extension_and_preserves_state(self):
        result = self.run_installer('--url', URL, '--no-start')
        self.assertIn('Finish setup:', result.stdout)
        self.assertTrue((self.prefix / 'bin/day-chime').stat().st_mode & 0o111)
        app = self.data / 'day-chime'
        manifest = json.loads((app / 'extension/manifest.json').read_text())
        self.assertEqual(manifest['content_scripts'][0]['matches'], [URL + '*'])
        self.assertIn(URL, (app / 'extension/site.js').read_text())
        config = self.config / 'day-chime/config.toml'
        original = config.read_text().replace('work_minutes = 480', 'work_minutes = 450')
        config.write_text(original)
        state = app / 'notifications.json'
        state.write_text('{"notified":["keep-me"]}')
        self.run_installer('--no-start')
        self.assertEqual(config.read_text(), original)
        self.assertEqual(state.read_text(), '{"notified":["keep-me"]}')

    def test_configure_registers_only_selected_extension_and_wrapper_uses_xdg(self):
        self.run_installer('--url', URL, '--no-start')
        self.run_installer('--browser', 'chrome', '--extension-id', ID, '--no-start', configure=True)
        host_path = self.config / 'google-chrome/NativeMessagingHosts/com.day_chime.attendance.json'
        host = json.loads(host_path.read_text())
        self.assertEqual(host['allowed_origins'], ['chrome-extension://' + ID + '/'])
        output = subprocess.check_output([host['path']], env=self.env, text=True).splitlines()
        self.assertEqual(output, [str(self.config), str(self.data)])
        unit = (self.config / 'systemd/user/day-chime.service').read_text()
        self.assertIn('ExecStart="' + str(self.prefix / 'bin/day-chime') + '"', unit)
        setup = self.prefix / 'bin/day-chime-setup'
        result = subprocess.run([str(setup), '--no-start'], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_setup_starts_service_only_with_extension_id(self):
        commands = self.root / 'systemctl.commands'
        tools = self.root / 'tools'
        tools.mkdir()
        systemctl = tools / 'systemctl'
        systemctl.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$DAY_CHIME_TEST_COMMANDS"\n')
        systemctl.chmod(0o755)
        self.env.update(PATH=str(tools) + os.pathsep + self.env['PATH'], DAY_CHIME_TEST_COMMANDS=str(commands))
        self.run_installer('--url', URL)
        self.assertFalse(commands.exists())
        self.run_installer('--extension-id', ID, configure=True)
        self.assertEqual(commands.read_text().splitlines(), ['--user daemon-reload', '--user enable --now day-chime.service', '--user restart day-chime.service'])

    def test_script_supports_curl_style_stdin_execution(self):
        arguments = ['sh', '-s', '--', '--prefix', str(self.prefix), '--archive', str(self.archive), '--checksum-file', str(self.checksum), '--url', URL, '--no-start']
        result = subprocess.run(arguments, input=INSTALLER.read_text(), env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.prefix / 'bin/day-chime-setup').is_file())

    def test_checksum_mismatch_installs_nothing(self):
        self.checksum.write_text('0' * 64 + '  ' + self.archive.name + '\n')
        result = self.run_installer('--url', URL, '--no-start', success=False)
        self.assertIn('checksum verification failed', result.stderr)
        self.assertFalse(self.prefix.exists())

    def test_archive_path_traversal_and_symlink_are_rejected(self):
        item = tarfile.TarInfo('../escaped')
        item.size = 4
        self.make_archive((item, io.BytesIO(b'evil')))
        self.run_installer('--url', URL, '--no-start', success=False)
        self.assertFalse((self.root / 'escaped').exists())
        item = tarfile.TarInfo('bin/link')
        item.type = tarfile.SYMTYPE
        item.linkname = '/tmp/escape'
        self.make_archive((item, None))
        self.run_installer('--url', URL, '--no-start', success=False)
        self.assertFalse(self.prefix.exists())

    def test_invalid_inputs_are_rejected_before_install(self):
        for arguments in [('--url', URL, '--extension-id', 'invalid'), ('--url', 'https://example.daouoffice.com:0/home'), ('--url', 'https://evil.example/home'), ('--url', URL, '--version', 'main'), ('--no-start',)]:
            with self.subTest(arguments=arguments):
                self.run_installer(*arguments, success=False)
                self.assertFalse(self.prefix.exists())

    def test_url_change_handles_indented_quoted_and_multiline_toml_values(self):
        for existing in ['  browser_url = "' + URL + '"', '"browser_url" = "' + URL + '"', "browser_url = " + chr(39) * 3 + URL + chr(39) * 3]:
            with self.subTest(existing=existing):
                self.run_installer('--url', URL, '--no-start')
                config = self.config / 'day-chime/config.toml'
                config.write_text('source="browser"\n' + existing + '\nwork_minutes=450\nbreak_minutes=45\ntimezone="Asia/Seoul"\n')
                self.run_installer('--url', 'https://another.daouoffice.com/home', '--no-start', configure=True)
                parsed = tomllib.loads(config.read_text())
                self.assertEqual(parsed['browser_url'], 'https://another.daouoffice.com/home')
                self.assertEqual(parsed['work_minutes'], 450)
                self.assertEqual(parsed['break_minutes'], 45)

    def test_explicit_url_change_preserves_other_config(self):
        self.run_installer('--url', URL, '--no-start')
        config = self.config / 'day-chime/config.toml'
        config.write_text(config.read_text().replace('break_minutes = 60', 'break_minutes = 45'))
        updated = 'https://another.daouoffice.com/home'
        self.run_installer('--url', updated, '--no-start', configure=True)
        self.assertEqual(tomllib.loads(config.read_text())['break_minutes'], 45)
        self.assertIn(updated, config.read_text())

    def make_custom_extension(self, directory, key='stable-public-key'):
        directory.mkdir(parents=True)
        manifest = {'manifest_version': 3, 'name': 'Day Chime — 근태 읽기', 'key': key,
                    'content_scripts': [{'matches': [URL + '*'], 'js': ['reader.js', 'content.js']}]}
        (directory / 'manifest.json').write_text(json.dumps(manifest))
        (directory / 'content.js').write_text('// old runtime')
        (directory / 'tests').mkdir()
        (directory / 'tests/personal.test.cjs').write_text('// unmanaged source test')
        (directory / 'notes.txt').write_text('keep personal notes')

    def test_custom_extension_update_preserves_key_unmanaged_files_and_remembers_path(self):
        custom = self.home / 'Projects/day-chime/extension'
        self.make_custom_extension(custom)
        self.run_installer('--url', URL, '--extension-dir', str(custom), '--no-start')
        app = self.data / 'day-chime'
        metadata = json.loads((app / 'install-state.json').read_text())
        self.assertEqual(metadata['extension_dir'], str(custom))
        self.assertEqual(metadata['canonical_extension_dir'], str(app / 'extension'))
        self.assertEqual((custom / 'content.js').read_text(), '// release content v1\n')
        self.assertEqual((custom / 'tests/personal.test.cjs').read_text(), '// unmanaged source test')
        self.assertEqual((custom / 'notes.txt').read_text(), 'keep personal notes')
        self.assertEqual(json.loads((custom / 'manifest.json').read_text())['key'], 'stable-public-key')
        self.make_archive(overrides={'extension/content.js': b'// release content v2\n', 'extension/new-runtime.js': b'// new runtime\n'})
        self.run_installer('--no-start')  # Reuse the actual loaded path from metadata.
        for directory in [custom, app / 'extension']:
            self.assertEqual((directory / 'content.js').read_text(), '// release content v2\n')
            self.assertEqual((directory / 'new-runtime.js').read_text(), '// new runtime\n')
            self.assertIn(URL, (directory / 'site.js').read_text())
        self.assertTrue((custom / 'tests/personal.test.cjs').is_file())
        updated = 'https://another.daouoffice.com/home'
        self.run_installer('--url', updated, '--no-start', configure=True)
        for directory in [custom, app / 'extension']:
            manifest = json.loads((directory / 'manifest.json').read_text())
            self.assertEqual(manifest['content_scripts'][0]['matches'], [updated + '*'])
            self.assertIn(updated, (directory / 'site.js').read_text())
        self.assertEqual(json.loads((custom / 'manifest.json').read_text())['key'], 'stable-public-key')

    def test_default_extension_metadata_and_unmanaged_files_are_preserved(self):
        self.run_installer('--url', URL, '--no-start')
        app = self.data / 'day-chime'
        metadata = json.loads((app / 'install-state.json').read_text())
        self.assertEqual(metadata['extension_dir'], str(app / 'extension'))
        (app / 'extension/notes.txt').write_text('keep')
        self.run_installer('--no-start')
        self.assertEqual((app / 'extension/notes.txt').read_text(), 'keep')

    def test_custom_extension_rejects_dangerous_paths_before_product_mutation(self):
        invalid = self.home / 'unrelated'
        invalid.mkdir()
        (invalid / 'manifest.json').write_text(json.dumps({'manifest_version': 3, 'name': 'Unrelated extension', 'content_scripts': []}))
        symlink = self.home / 'linked'
        symlink.symlink_to(invalid, target_is_directory=True)
        targets = [self.home, self.prefix, self.prefix / 'bin', self.config, self.data,
                   self.data / 'day-chime', self.data / 'day-chime/nested',
                   self.data / 'day-chime/extension/nested', invalid, symlink, symlink / 'extension']
        for target in targets:
            with self.subTest(target=target):
                self.run_installer('--url', URL, '--extension-dir', str(target), '--no-start', success=False)
                self.assertFalse(self.prefix.exists())
                self.assertFalse(self.config.exists())
                self.assertFalse(self.data.exists())
        self.assertEqual(json.loads((invalid / 'manifest.json').read_text())['name'], 'Unrelated extension')

    def test_custom_extension_with_symlink_entry_is_rejected_without_changes(self):
        custom = self.home / 'extension'
        self.make_custom_extension(custom)
        outside = self.home / 'outside.txt'
        outside.write_text('unchanged')
        (custom / 'linked.txt').symlink_to(outside)
        before = (custom / 'content.js').read_text()
        self.run_installer('--url', URL, '--extension-dir', str(custom), '--no-start', success=False)
        self.assertEqual((custom / 'content.js').read_text(), before)
        self.assertEqual(outside.read_text(), 'unchanged')
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.data.exists())

    def test_invalid_release_manifest_is_rejected_before_binaries_are_replaced(self):
        self.make_archive(overrides={'extension/manifest.json': b'{"manifest_version":3,"name":"Other","content_scripts":[]}'})
        self.run_installer('--url', URL, '--no-start', success=False)
        self.assertFalse(self.prefix.exists())
        self.assertFalse((self.config / 'day-chime/config.toml').exists())

    def test_shared_installer_lock_rejects_concurrent_installation(self):
        app = self.data / 'day-chime'
        app.mkdir(parents=True)
        with (app / 'installer.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_installer('--url', URL, '--no-start', success=False)
        self.assertIn('Another Day Chime installer is running', result.stderr)
        self.assertFalse(self.prefix.exists())
        self.assertFalse((self.config / 'day-chime/config.toml').exists())

    def test_no_downgrade_checks_installed_binary_before_any_replacement(self):
        self.run_installer('--url', URL, '--no-start')
        binary = self.prefix / 'bin/day-chime'
        binary.write_text("#!/bin/sh\nprintf 'day-chime 9.0.0\\n'\n")
        before_binary = binary.read_bytes()
        app = self.data / 'day-chime'
        before_metadata = (app / 'install-state.json').read_bytes()
        before_manifest = (app / 'extension/manifest.json').read_bytes()
        result = self.run_installer('--no-downgrade', '--no-start', success=False)
        self.assertIn('refusing to downgrade', result.stderr)
        self.assertEqual(binary.read_bytes(), before_binary)
        self.assertEqual((app / 'install-state.json').read_bytes(), before_metadata)
        self.assertEqual((app / 'extension/manifest.json').read_bytes(), before_manifest)
        self.run_installer('--no-start')  # Explicit direct installer still permits a downgrade.
        self.assertNotEqual(binary.read_bytes(), before_binary)

    def test_updater_preserves_custom_service_and_legacy_native_host(self):
        self.run_installer('--url', URL, '--no-start')
        unit = self.config / 'systemd/user/day-chime.service'
        custom_unit = unit.read_text() + '\nEnvironmentFile=-/custom/credentials.env\n# user customization\n'
        unit.write_text(custom_unit)
        native = self.config / 'BraveSoftware/Brave-Browser/NativeMessagingHosts/com.day_chime.attendance.json'
        native.parent.mkdir(parents=True)
        native.write_text('{"path":"/legacy/host","allowed_origins":["chrome-extension://' + ID + '/"]}')
        before = native.read_bytes()
        self.run_installer('--preserve-service', '--no-downgrade', '--no-start')
        self.assertEqual(unit.read_text(), custom_unit)
        self.assertEqual(native.read_bytes(), before)


    def test_release_public_key_never_changes_existing_extension_identity(self):
        custom = self.home / 'extension'
        self.make_custom_extension(custom, key='existing-public-key')
        incoming = {'manifest_version': 3, 'name': 'Day Chime — 근태 읽기', 'key': 'different-release-key',
                    'content_scripts': [{'matches': [URL + '*'], 'js': ['site.js', 'reader.js', 'content.js']}]}
        self.make_archive(overrides={'extension/manifest.json': json.dumps(incoming).encode()})
        self.run_installer('--url', URL, '--extension-dir', str(custom), '--no-start')
        self.assertEqual(json.loads((custom / 'manifest.json').read_text())['key'], 'existing-public-key')
        self.assertEqual(json.loads((self.data / 'day-chime/extension/manifest.json').read_text())['key'], 'different-release-key')

    def test_configuration_changed_during_download_aborts_before_binary_replacement(self):
        self.run_installer('--url', URL, '--no-start')
        binary = self.prefix / 'bin/day-chime'
        before = binary.read_bytes()
        config_path = self.config / 'day-chime/config.toml'
        tools = self.root / 'tools'
        tools.mkdir()
        fake_curl = tools / 'curl'
        # Simulate an editor saving while the release is downloaded. Curl only
        # copies the local verified fixture; no network or actual installation.
        fake_curl.write_text("""#!/usr/bin/env python3
import os, pathlib, shutil, sys
args=sys.argv[1:]
pathlib.Path(os.environ['TEST_CONFIG_PATH']).write_text('browser_url = "https://changed.daouoffice.com/home"\\n')
destination=pathlib.Path(args[args.index('--output')+1])
source=pathlib.Path(os.environ['TEST_CHECKSUM'] if destination.name=='SHA256SUMS' else os.environ['TEST_ARCHIVE'])
shutil.copyfile(source,destination)
""")
        fake_curl.chmod(0o755)
        env = dict(self.env, PATH=str(tools) + os.pathsep + self.env['PATH'],
                   TEST_CONFIG_PATH=str(config_path), TEST_ARCHIVE=str(self.archive), TEST_CHECKSUM=str(self.checksum))
        result = subprocess.run(['sh', str(INSTALLER), '--prefix', str(self.prefix), '--version', 'v0.4.0', '--no-start'], env=env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('changed during installation', result.stderr)
        self.assertEqual(binary.read_bytes(), before)
        self.assertIn('changed.daouoffice.com', config_path.read_text())


    def test_no_downgrade_rejects_unknown_binary_version_despite_stale_metadata(self):
        self.run_installer('--url', URL, '--no-start')
        binary = self.prefix / 'bin/day-chime'
        binary.write_text("#!/bin/sh\nprintf 'unrecognized binary\\n'\n")
        before = binary.read_bytes()
        result = self.run_installer('--no-downgrade', '--no-start', success=False)
        self.assertIn('Cannot establish installed binary version safely', result.stderr)
        self.assertEqual(binary.read_bytes(), before)



if __name__ == '__main__':
    unittest.main()
