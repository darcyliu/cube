import copy
import datetime
import hashlib
import importlib.util
import os
import subprocess
import tempfile
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('signing', Path(__file__).with_name('validate-signing.py'))
signing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signing)


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.entitlements = {
            'com.apple.application-identifier': 'TEAM.net.example.cube',
            'com.apple.developer.team-identifier': 'TEAM',
            'com.apple.vm.networking': True,
        }
        self.profile = {
            'Name': 'Distribution', 'TeamIdentifier': ['TEAM'],
            'ExpirationDate': datetime.datetime.now() + datetime.timedelta(days=30),
            'ProvisionsAllDevices': True,
            'Entitlements': copy.deepcopy(self.entitlements),
            'DeveloperCertificates': [b'authorized certificate'],
        }
        self.fingerprint = hashlib.sha1(b'authorized certificate').hexdigest().upper()
        self.name = 'Developer ID Application: Example (TEAM)'

    def validate(self):
        signing.validate_profile(self.profile, 'net.example.cube', 'TEAM', self.entitlements, True)

    def test_valid_distribution(self):
        self.validate()
        self.assertEqual(signing.select_identity(self.profile, [(self.fingerprint, self.name)], distribution=True), self.fingerprint)

    def test_same_team_different_certificate_rejected(self):
        with self.assertRaisesRegex(ValueError, 'No matching signing identity'):
            signing.select_identity(self.profile, [('0' * 40, self.name)], distribution=True)

    def test_explicit_identity_cannot_bypass_certificate_match(self):
        with self.assertRaises(ValueError):
            signing.select_identity(self.profile, [('0' * 40, self.name)], self.name, True)

    def test_development_identity_rejected_for_distribution(self):
        with self.assertRaises(ValueError):
            signing.select_identity(self.profile, [(self.fingerprint, 'Apple Development: Example')], distribution=True)

    def test_development_profile_rejected(self):
        self.profile['ProvisionedDevices'] = ['device']
        with self.assertRaisesRegex(ValueError, 'Developer ID provisioning profile'):
            self.validate()

    def test_missing_or_false_networking_rejected(self):
        for value in (None, False):
            self.profile['Entitlements']['com.apple.vm.networking'] = value
            with self.assertRaisesRegex(ValueError, 'does not authorize'):
                self.validate()

    def test_expired_profile_rejected(self):
        self.profile['ExpirationDate'] = datetime.datetime(2000, 1, 1)
        with self.assertRaisesRegex(ValueError, 'expired'):
            self.validate()

    def test_wrong_entitlement_identifiers_rejected(self):
        for key in ('com.apple.application-identifier', 'com.apple.developer.team-identifier'):
            with self.subTest(key=key):
                original = self.entitlements[key]
                self.entitlements[key] = 'OTHER'
                with self.assertRaises(ValueError):
                    self.validate()
                self.entitlements[key] = original


class SigningShellTests(unittest.TestCase):
    def test_system_bash_reaches_validator_in_both_modes(self):
        # Exercise the real shell entry point, without Keychain access or signing.
        root = Path(__file__).resolve().parent.parent
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            app = temp / 'Cube.app'
            executable = app / 'Contents/MacOS/CubeBar'
            executable.parent.mkdir(parents=True)
            executable.touch()
            stub = temp / 'python3'
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >&2\nexit 23\n')
            stub.chmod(0o755)
            for mode in ('', '1'):
                with self.subTest(distribution=mode):
                    env = dict(os.environ, PATH=f'{temp}:/usr/bin:/bin',
                               APP_DIR=str(app), CUBE_BIN=str(executable),
                               PROFILE=str(root / 'Cube_Dev.provisionprofile'),
                               IDENTITY='', DIST=mode,
                               ENTITLEMENTS=str(root / 'app/Cube/Resources/Cube.networking.entitlements'))
                    result = subprocess.run(['/bin/bash', str(root / 'scripts/sign-cube-networking.sh')],
                                            env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 23, result.stderr)
                    self.assertIn('--profile', result.stderr)
                    self.assertEqual('--distribution' in result.stderr, mode == '1')
                    self.assertNotIn('unbound variable', result.stderr)


if __name__ == '__main__':
    unittest.main()
