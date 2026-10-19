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
        if event.get('reason') != 'compiler-artifact' or not event.get('profile', {}).get('test') or not event.get('executable'):
            continue
        source = Path(event['target']['src_path']).resolve()
        role = None
        if source.is_relative_to((root / 'crates/gents-client-host').resolve()):
            role = 'host'
        elif source.is_relative_to((root / 'crates/gents-desktop-bridge').resolve()) and 'lib' in event['target']['kind']:
            role = 'bridge'
        if role is None:
            continue
        binary = Path(event['executable']).resolve()
        binary.relative_to(target.resolve())
        found.append({'role': role, 'name': event['target']['name'], 'path': str(binary), 'cwd': str(root / 'crates' / ('gents-client-host' if role == 'host' else 'gents-desktop-bridge')), 'sha256': digest(binary)})
    if (sum(item['role'] == 'bridge' for item in found) != 1
            or not any(item['role'] == 'host' and item['name'] == 'runtime' for item in found)
            or len({item['path'] for item in found}) != len(found)):
        raise ValueError('native transport test targets are missing or duplicated')
    return found


def compile_tests(root, manifest):
    log = manifest.with_suffix('.jsonl')
    with log.open('w') as output:
        subprocess.run(['cargo', 'test', '--locked', '-p', 'gents-client-host',
                        '-p', 'gents-desktop-bridge', '--no-default-features',
                        '--features', 'gents-client-host/runtime-e2e', '--no-run',
                        '--message-format=json', '--timings', '--target-dir', str(root / 'target')], cwd=root, stdout=output, check=True)
    manifest.write_text(json.dumps(artifacts(log, root, root / 'target'), indent=2) + '\n')


def command(item):
    return [item['path'], *(['host::local::windows'] if item['role'] == 'bridge' else []), '--test-threads=2']


def check_result(item, status, content):
    summary = re.findall(r'test result: ok\. (\d+) passed; 0 failed;', content)
    if status != 0 or len(summary) != 1:
        raise RuntimeError('native transport tests failed')
    if (item['role'] == 'bridge' or item['name'] == 'runtime') and int(summary[0]) == 0:
        raise RuntimeError('required native transport cases did not execute')


def run_tests(manifest, role):
    selected = [item for item in json.loads(manifest.read_text()) if item['role'] == role]
    if not selected:
        raise ValueError('no native transport test artifacts selected')
    for item in selected:
        if digest(Path(item['path'])) != item['sha256']:
            raise ValueError('compiled native transport test artifact changed')
        with tempfile.TemporaryFile(mode='w+') as output:
            options = {} if os.name == 'nt' else {'start_new_session': True}
            env = dict(os.environ, CARGO_MANIFEST_DIR=item['cwd'])
            env['PATH'] = str(Path(item['path']).parent) + os.pathsep + env.get('PATH', '')
            child = subprocess.Popen(command(item), cwd=item['cwd'], env=env, stdout=output, stderr=subprocess.STDOUT, **options)
            try:
                status = child.wait(timeout=900)
            except subprocess.TimeoutExpired:
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/T', '/F', '/PID', str(child.pid)], check=False)
                else:
                    os.killpg(child.pid, signal.SIGKILL)
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
    args = parser.parse_args()
    if args.phase == 'compile':
        compile_tests(Path(__file__).resolve().parents[2], args.manifest)
    else:
        run_tests(args.manifest, args.phase)


if __name__ == '__main__':
    main()
