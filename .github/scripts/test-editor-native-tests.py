#!/usr/bin/env python3
"""Offline artifact selection checks; no Cargo or native processes."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('editor', Path(__file__).with_name('editor-native-tests.py'))
e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = self.root / 'target'
        self.target.mkdir()
        self.log = self.root / 'build.jsonl'

    def event(self, package, name, kind='lib'):
        binary = self.target / name
        binary.write_bytes(name.encode())
        return {'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind],
                'src_path': str(self.root / 'crates' / package / 'src/lib.rs')},
                'profile': {'test': True}, 'executable': str(binary)}

    def select(self, events):
        self.log.write_text(''.join(json.dumps(event) + '\n' for event in events))
        return e.artifacts(self.log, self.root, self.target)

    def test_exact_bridge_and_host_runtime_are_required(self):
        bridge = self.event('gents-desktop-bridge', 'gents_desktop_bridge')
        runtime = self.event('gents-client-host', 'runtime', 'test')
        main = self.event('gents-client-host', 'gents-client-host', 'bin')
        dependency = self.event('unrelated', 'other')
        found = self.select([bridge, runtime, main, dependency])
        self.assertEqual(len(found), 3)
        self.assertEqual(e.command(found[0])[1], 'host::local::windows')
        self.assertNotIn('cargo', e.command(found[1]))
        for events in ([], [runtime], [bridge], [bridge, runtime, runtime]):
            with self.assertRaises(ValueError):
                self.select(events)

    def test_non_test_artifacts_are_ignored_and_paths_cannot_escape_target(self):
        bridge = self.event('gents-desktop-bridge', 'bridge')
        runtime = self.event('gents-client-host', 'runtime', 'test')
        normal = self.event('gents-client-host', 'normal')
        normal['profile']['test'] = False
        self.assertEqual(len(self.select([bridge, runtime, normal])), 2)
        runtime['executable'] = str(self.root / 'outside')
        with self.assertRaises(ValueError):
            self.select([bridge, runtime])

    def test_compile_uses_one_feature_graph_and_no_test_execution(self):
        manifest = self.root / 'manifest.json'
        with patch.object(e.subprocess, 'run') as run, patch.object(e, 'artifacts', return_value=[]):
            e.compile_tests(self.root, manifest)
        command = run.call_args.args[0]
        self.assertIn('--no-run', command)
        self.assertIn('gents-client-host/runtime-e2e', command)
        self.assertIn('gents-desktop-bridge', command)
        self.assertIn('--no-default-features', command)
        self.assertNotIn('--all-features', command)
        self.assertEqual(run.call_count, 1)

    def test_result_rejects_false_green_and_zero_required_cases(self):
        runtime = {'role': 'host', 'name': 'runtime'}
        output = 'test result: ok. 2 passed; 0 failed; 0 ignored;'
        e.check_result(runtime, 0, output)
        for status, content in [(1, output), (0, ''), (0, output + output),
                                (0, 'test result: ok. 0 passed; 0 failed;')]:
            with self.assertRaises(RuntimeError):
                e.check_result(runtime, status, content)
        e.check_result({'role': 'host', 'name': 'gents-client-host'}, 0,
                       'test result: ok. 0 passed; 0 failed;')

    def test_changed_or_missing_artifacts_fail_before_execution(self):
        manifest = self.root / 'manifest.json'
        item = {'role': 'host', 'name': 'runtime', 'path': str(self.target / 'runtime'), 'sha256': '0' * 64}
        (self.target / 'runtime').write_bytes(b'changed')
        manifest.write_text(json.dumps([item]))
        with patch.object(e.subprocess, 'Popen') as run:
            with self.assertRaises(ValueError):
                e.run_tests(manifest, 'host')
            run.assert_not_called()
        manifest.write_text('[]')
        with self.assertRaises(ValueError):
            e.run_tests(manifest, 'bridge')


if __name__ == '__main__':
    unittest.main()
