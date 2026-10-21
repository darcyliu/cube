#!/usr/bin/env python3
"""Fail closed on provisioning-profile/signing-certificate mismatches."""
import argparse
import datetime
import fnmatch
import hashlib
import plistlib
import re
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*args):
    return subprocess.check_output(args, stderr=subprocess.PIPE)


def decode_profile(path):
    # Decode the CMS container using Security, never scrape unverified XML bytes.
    return plistlib.loads(run('security', 'cms', '-D', '-i', str(path)))


def validate_profile(profile, bundle_id, team_id, entitlements, distribution=False):
    expiration = profile['ExpirationDate'].replace(tzinfo=datetime.timezone.utc)
    if expiration <= datetime.datetime.now(datetime.timezone.utc):
        raise ValueError('Provisioning profile has expired')
    allowed = profile.get('Entitlements', {})
    if team_id not in profile.get('TeamIdentifier', []):
        raise ValueError(f'Profile does not belong to team {team_id}')
    if distribution and (not profile.get('ProvisionsAllDevices') or profile.get('ProvisionedDevices')):
        raise ValueError('DMG distribution requires a Developer ID provisioning profile')
    app_id = entitlements.get('com.apple.application-identifier', '')
    if not app_id.endswith('.' + bundle_id):
        raise ValueError(f'Application identifier does not match bundle ID {bundle_id}')
    for key in ('com.apple.application-identifier', 'com.apple.developer.team-identifier', 'com.apple.vm.networking'):
        actual, expected = entitlements.get(key), allowed.get(key)
        matches = (isinstance(actual, str) and isinstance(expected, str)
                   and fnmatch.fnmatchcase(actual, expected)) if key == 'com.apple.application-identifier' else actual == expected
        if actual is None or not matches:
            raise ValueError(f'Profile does not authorize entitlement {key}: {actual!r}')
    if entitlements['com.apple.developer.team-identifier'] != team_id:
        raise ValueError('Entitlement team identifier does not match signing team')
    if entitlements['com.apple.vm.networking'] is not True:
        raise ValueError('VM Networking must be enabled')
    if distribution and (entitlements.get('get-task-allow') or entitlements.get('com.apple.security.get-task-allow')):
        raise ValueError('Distribution entitlements must not enable get-task-allow')


def certificate_hashes(profile):
    return {hashlib.sha1(cert).hexdigest().upper() for cert in profile['DeveloperCertificates']}


def select_identity(profile, identities, requested='', distribution=False):
    authorized = certificate_hashes(profile)
    for fingerprint, name in identities:
        if requested and requested not in (fingerprint, name):
            continue
        if fingerprint not in authorized:
            continue
        if distribution and not name.startswith('Developer ID Application:'):
            continue
        return fingerprint
    raise ValueError('No matching signing identity authorized by profile '
                     f'{profile.get("Name", "")} (certificate SHA-1: {", ".join(sorted(authorized))}). '
                     'Regenerate the Developer ID profile with the installed Developer ID Application '
                     'certificate, or install the matching certificate and private key. '
                     'A different certificate from the same team is not sufficient.')


def validate_app(app):
    run('codesign', '--verify', '--deep', '--strict', str(app))
    for bundle in (app, app / 'Contents/Helpers/cube.app'):
        profile = decode_profile(bundle / 'Contents/embedded.provisionprofile')
        info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
        entitlements = plistlib.loads(run('codesign', '-d', '--entitlements', '-', '--xml', str(bundle)))
        details = subprocess.run(['codesign', '-dvv', str(bundle)], capture_output=True, text=True, check=True).stderr
        team = re.search(r'^TeamIdentifier=(.+)$', details, re.M)
        if not team or 'Authority=Developer ID Application:' not in details or '(runtime)' not in details:
            raise ValueError(f'{bundle}: requires Developer ID Application signing and hardened runtime')
        validate_profile(profile, info['CFBundleIdentifier'], team[1], entitlements, True)
        with tempfile.TemporaryDirectory() as temp:
            prefix = str(Path(temp) / 'cert')
            run('codesign', '-d', '--extract-certificates=' + prefix, str(bundle))
            fingerprint = hashlib.sha1(Path(prefix + '0').read_bytes()).hexdigest().upper()
        if fingerprint not in certificate_hashes(profile):
            raise ValueError(f'{bundle}: signing certificate {fingerprint} is not authorized by embedded profile {profile.get("Name")}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, help='Validate an existing app before distribution')
    parser.add_argument('--profile')
    parser.add_argument('--bundle-id', default='net.neuralengine.cube')
    parser.add_argument('--team-id', default='ZV8TTD4Z45')
    parser.add_argument('--entitlements', action='append', default=[])
    parser.add_argument('--identity', default='')
    parser.add_argument('--distribution', action='store_true')
    args = parser.parse_args()
    try:
        if args.app:
            validate_app(args.app)
            return
        if not args.profile or not args.entitlements:
            parser.error('--profile and --entitlements are required without --app')
        profile = decode_profile(args.profile)
        for path in args.entitlements:
            validate_profile(profile, args.bundle_id, args.team_id,
                             plistlib.loads(Path(path).read_bytes()), args.distribution)
        output = run('security', 'find-identity', '-v', '-p', 'codesigning').decode()
        identities = re.findall(r'\b([0-9A-F]{40}) "([^"]+)"', output)
        print(select_identity(profile, identities, args.identity, args.distribution))
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f'[signing] ERROR: {error}', file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.decode(errors='replace'), file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
