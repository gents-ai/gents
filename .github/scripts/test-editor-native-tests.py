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

    def event(self, package, name, kind='lib', testing=True):
        binary = self.target / ('tests' if testing else 'bin') / name
        binary.parent.mkdir(exist_ok=True)
        binary.write_bytes(name.encode())
        return {'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind],
                'src_path': str(self.root / 'crates' / package / 'src/lib.rs')},
                'profile': {'test': testing}, 'executable': str(binary)}

    def select(self, events):
        self.log.write_text(''.join(json.dumps(event) + '\n' for event in events))
        return e.artifacts(self.log, self.root, self.target)

    def required(self):
        return [self.event('gents', 'gents'),
                self.event('gents-desktop-bridge', 'gents_desktop_bridge'),
                self.event('gents-client-host', 'runtime', 'test'),
                self.event('gents-cli', 'port_allocation', 'test'),
                self.event('gents-cli', 'gents', 'bin', False),
                self.event('gents-client-host', 'gents-client-host', 'bin', False)]

    def test_exact_bridge_runtime_and_production_binaries_are_required(self):
        events = self.required()
        main = self.event('gents-client-host', 'gents-client-host', 'bin')
        found = self.select([*events, main, self.event('unrelated', 'other')])
        self.assertEqual(len(found), 7)
        bridge = next(item for item in found if item['role'] == 'bridge')
        self.assertEqual(e.command(bridge)[1], 'host::local::windows')
        self.assertNotIn('cargo', e.command(found[-1]))
        for invalid in ([], events[:-1], events[1:], [*events, events[1]]):
            with self.assertRaises(ValueError):
                self.select(invalid)

    def test_non_test_libraries_are_ignored_and_paths_cannot_escape_target(self):
        events = self.required()
        normal = self.event('gents-client-host', 'normal', testing=False)
        self.assertEqual(len(self.select([*events, normal])), 6)
        events[1]['executable'] = str(self.root / 'outside')
        with self.assertRaises(ValueError):
            self.select(events)

    def test_target_discovery_includes_new_host_tests_and_respects_features(self):
        targets = [{'name': 'gents-client-host', 'kind': ['bin']},
                   {'name': 'metadata', 'kind': ['test']},
                   {'name': 'runtime_cli', 'kind': ['test']},
                   {'name': 'runtime', 'kind': ['test'], 'required-features': ['runtime-e2e']},
                   {'name': 'editor', 'kind': ['test'], 'required-features': ['editor-e2e']}]
        metadata = {'packages': [{'name': 'gents-client-host', 'targets': targets}]}
        with patch.object(e.subprocess, 'run') as run:
            run.return_value.stdout = json.dumps(metadata)
            self.assertEqual(e.transport_targets(self.root), ['metadata', 'runtime', 'runtime_cli'])
            self.assertIn('--no-deps', run.call_args.args[0])

    def test_compile_uses_one_feature_graph_and_no_test_execution(self):
        manifest = self.root / 'manifest.json'
        with patch.object(e.subprocess, 'run') as run, patch.object(e, 'artifacts', return_value=[]), \
                patch.object(e, 'transport_targets', return_value=['metadata', 'runtime', 'runtime_cli']):
            e.compile_tests(self.root, manifest)
        command = run.call_args.args[0]
        self.assertIn('--no-run', command)
        self.assertIn('gents-client-host/runtime-e2e', command)
        self.assertIn('gents-desktop-bridge', command)
        self.assertIn('gents-cli', command)
        self.assertIn('gents', command)
        targets = [command[i + 1] for i, value in enumerate(command[:-1]) if value == '--test']
        self.assertEqual(targets, ['port_allocation', 'metadata', 'runtime', 'runtime_cli'])
        self.assertIn('--no-default-features', command)
        self.assertIn('--message-format=json-render-diagnostics', command)
        self.assertNotIn('--all-features', command)
        self.assertEqual(run.call_count, 1)

    def test_compile_forwards_the_native_ci_profile_without_changing_targets(self):
        manifest = self.root / 'manifest.json'
        with patch.object(e.subprocess, 'run') as run, patch.object(e, 'artifacts', return_value=[]), \
                patch.object(e, 'transport_targets', return_value=['runtime']):
            e.compile_tests(self.root, manifest, profile='ci-native')
        command = run.call_args.args[0]
        self.assertEqual(command[command.index('--profile') + 1], 'ci-native')
        self.assertIn('--no-run', command)
        self.assertIn('--lib', command)
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
        items = self.select(self.required())
        runtime = next(item for item in items if item['role'] == 'host')
        Path(runtime['path']).write_bytes(b'changed')
        manifest.write_text(json.dumps(items))
        with patch.object(e.subprocess, 'Popen') as run:
            with self.assertRaises(ValueError):
                e.run_tests(manifest, 'host')
            run.assert_not_called()
        items = self.select(self.required())
        binary = next(item for item in items if item['role'] == 'binary')
        Path(binary['path']).write_bytes(b'changed')
        manifest.write_text(json.dumps(items))
        with patch.object(e.subprocess, 'Popen') as run:
            with self.assertRaises(ValueError):
                e.run_tests(manifest, 'bridge')
            run.assert_not_called()
        manifest.write_text('[]')
        with self.assertRaises(ValueError):
            e.run_tests(manifest, 'bridge')

    def test_process_phase_runs_each_exact_windows_job_case(self):
        manifest = self.root / 'manifest.json'
        manifest.write_text(json.dumps(self.select(self.required())))
        with patch.object(e.subprocess, 'Popen') as run, patch.object(e, 'check_result'):
            run.return_value.wait.return_value = 0
            e.run_tests(manifest, 'process')
        self.assertEqual(run.call_count, 2)
        self.assertEqual([call.args[0][1:] for call in run.call_args_list],
                         [[name, '--exact'] for name in e.PROCESS_TESTS])
        for call in run.call_args_list:
            self.assertEqual(call.kwargs['cwd'], str(self.root / 'crates/gents'))
        with self.assertRaises(RuntimeError):
            e.check_result({'role': 'process', 'name': 'gents'}, 0,
                           'test result: ok. 0 passed; 0 failed;')

    def test_host_phase_runs_the_cli_port_fixture_with_verified_native_binaries(self):
        manifest = self.root / 'manifest.json'
        manifest.write_text(json.dumps(self.select(self.required())))
        with patch.object(e.subprocess, 'Popen') as run, patch.object(e, 'check_result'):
            run.return_value.wait.return_value = 0
            e.run_tests(manifest, 'host')
        self.assertEqual(run.call_count, 2)
        self.assertEqual({call.kwargs['cwd'] for call in run.call_args_list},
                         {str(self.root / 'crates/gents-cli'), str(self.root / 'crates/gents-client-host')})
        for call in run.call_args_list:
            self.assertEqual(call.kwargs['env']['GENTS_NATIVE_TEST_BIN_DIR'], str((self.target / 'bin').resolve()))


if __name__ == '__main__':
    unittest.main()
