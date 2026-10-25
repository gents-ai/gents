#!/usr/bin/env python3
"""Compile the Windows transport test graph once and execute its exact artifacts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def artifacts(log, root, target):
    found = []
    for line in log.read_text().splitlines():
        event = json.loads(line)
        if event.get('reason') != 'compiler-artifact' or not event.get('executable'):
            continue
        source = Path(event['target']['src_path']).resolve()
        package = next((name for name in ('gents-client-host', 'gents-desktop-bridge', 'gents-cli')
                        if source.is_relative_to((root / 'crates' / name).resolve())), None)
        testing = event.get('profile', {}).get('test')
        name = event['target']['name']
        kind = event['target']['kind']
        role = None
        if not testing and 'bin' in kind and (package, name) in (
                ('gents-cli', 'gents'), ('gents-client-host', 'gents-client-host')):
            role = 'binary'
        elif testing and package == 'gents-client-host':
            role = 'host'
        elif testing and package == 'gents-desktop-bridge' and 'lib' in kind:
            role = 'bridge'
        elif testing and package == 'gents-cli' and name == 'port_allocation' and 'test' in kind:
            role = 'cli'
        if role is None:
            continue
        binary = Path(event['executable']).resolve()
        binary.relative_to(target.resolve())
        found.append({'role': role, 'name': name, 'path': str(binary), 'cwd': str(root / 'crates' / package), 'sha256': digest(binary)})
    if (sum(item['role'] == 'bridge' for item in found) != 1
            or not any(item['role'] == 'host' and item['name'] == 'runtime' for item in found)
            or not any(item['role'] == 'cli' for item in found)
            or sorted(item['name'] for item in found if item['role'] == 'binary') != ['gents', 'gents-client-host']
            or len({item['path'] for item in found}) != len(found)):
        raise ValueError('native transport test targets are missing or duplicated')
    return sorted(found, key=lambda item: (item['role'], item['name']))


def transport_targets(root):
    metadata = subprocess.run(['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'],
                              cwd=root, capture_output=True, text=True, check=True)
    package = next(item for item in json.loads(metadata.stdout)['packages'] if item['name'] == 'gents-client-host')
    targets = sorted(item['name'] for item in package['targets']
                     if 'test' in item['kind'] and set(item.get('required-features') or ()) <= {'runtime-e2e'})
    if 'runtime' not in targets:
        raise ValueError('native runtime test target is missing')
    return targets


def compile_tests(root, manifest, profile='test'):
    """Cargo builds package binaries alongside selected integration targets."""
    log = manifest.with_suffix('.jsonl')
    targets = [flag for name in ['port_allocation', *transport_targets(root)] for flag in ('--test', name)]
    with log.open('w') as output:
        subprocess.run(['cargo', 'test', '--locked', '-p', 'gents-client-host',
                        '-p', 'gents-desktop-bridge', '-p', 'gents-cli', '--no-default-features',
                        '--features', 'gents-client-host/runtime-e2e', '--profile', profile, '--lib', *targets, '--no-run',
                        '--message-format=json-render-diagnostics', '--timings', '--target-dir', str(root / 'target')], cwd=root, stdout=output, check=True)
    manifest.write_text(json.dumps(artifacts(log, root, root / 'target'), indent=2) + '\n')


def command(item):
    return [item['path'], *(['host::local::windows'] if item['role'] == 'bridge' else [])]


def check_result(item, status, content):
    summary = re.findall(r'test result: ok\. (\d+) passed; 0 failed;', content)
    if status != 0 or len(summary) != 1:
        raise RuntimeError('native transport tests failed')
    if (item['role'] in ('bridge', 'cli') or item['name'] == 'runtime') and int(summary[0]) == 0:
        raise RuntimeError('required native transport cases did not execute')


def run_tests(manifest, role):
    items = json.loads(manifest.read_text())
    binaries = [item for item in items if item['role'] == 'binary']
    if sorted(item['name'] for item in binaries) != ['gents', 'gents-client-host']:
        raise ValueError('native runtime binaries are missing or duplicated')
    for item in binaries:
        if digest(Path(item['path'])) != item['sha256']:
            raise ValueError('compiled native runtime binary changed')
    binary_dir = Path(next(item['path'] for item in binaries if item['name'] == 'gents-client-host')).parent
    selected = [item for item in items if item['role'] == role or (role == 'host' and item['role'] == 'cli')]
    if not selected:
        raise ValueError('no native transport test artifacts selected')
    for item in selected:
        if digest(Path(item['path'])) != item['sha256']:
            raise ValueError('compiled native transport test artifact changed')
    for item in selected:
        with tempfile.TemporaryFile(mode='w+') as output:
            options = {} if os.name == 'nt' else {'start_new_session': True}
            env = dict(os.environ, CARGO_MANIFEST_DIR=item['cwd'])
            env['GENTS_NATIVE_TEST_BIN_DIR'] = str(binary_dir)
            env['PATH'] = str(binary_dir) + os.pathsep + str(Path(item['path']).parent) + os.pathsep + env.get('PATH', '')
            child = subprocess.Popen(command(item), cwd=item['cwd'], env=env, stdout=output, stderr=subprocess.STDOUT, **options)
            try:
                status = child.wait(timeout=900)
            except subprocess.TimeoutExpired:
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/T', '/F', '/PID', str(child.pid)], check=False)
                else:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                child.wait()
                raise
            finally:
                output.seek(0)
                content = output.read()
                print(content, end='', flush=True)
        check_result(item, status, content)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('compile', 'host', 'bridge'))
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--profile', default='test')
    args = parser.parse_args()
    if args.phase == 'compile':
        compile_tests(Path(__file__).resolve().parents[2], args.manifest, args.profile)
    else:
        run_tests(args.manifest, args.phase)


if __name__ == '__main__':
    main()
