"""Disposable installer contract tests: never use the real user's installation."""
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

    def make_archive(self, extra=None):
        entries = {
            'bin/day-chime': b'#!/bin/sh\nprintf "fixture-day-chime\\n"\n',
            'bin/day-chime-browser-host': b'#!/bin/sh\nprintf "%s\\n" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"\n',
            'extension/manifest.json': json.dumps({'manifest_version': 3, 'content_scripts': [{'matches': [URL + '*'], 'js': ['site.js', 'reader.js', 'content.js']}]}).encode(),
            'extension/site.js': b'globalThis.DayChimeSite = {sourceUrl: "https://example.daouoffice.com/home"};\n',
            'assets/day-chime-128.png': b'icon-fixture',
            'deploy/systemd/day-chime.service': b'fixture',
            'config.example.toml': b'source="browser"\n',
            'LICENSE': b'MIT fixture',
            'README.md': b'fixture readme',
            'install.sh': INSTALLER.read_bytes(),
            'release.json': json.dumps({'version': 'v0.4.0', 'platform': 'linux-x86_64'}).encode(),
        }
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


if __name__ == '__main__':
    unittest.main()
