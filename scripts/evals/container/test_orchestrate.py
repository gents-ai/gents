import io
import json
import os
import pathlib
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import orchestrate  # noqa: E402

TARGET = {"inference_backends": [{"endpoint": "http://127.0.0.1:18781/v1"}],
          "inference_profiles": [{"model_name": "inkling-2-mini"}]}


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value if isinstance(value, str) else json.dumps(value))


class HostileFiles(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        self.secret = self.root / "secret"
        self.secret.write_text("host file")

    def test_reads_follow_no_link_and_take_only_small_regular_files(self):
        export = self.root / "export"
        write(export / "result.json", {"exit_code": 0})
        (export / "linked.json").symlink_to(self.secret)
        (export / "big.json").write_bytes(b" " * 100)
        (export / "dir.json").mkdir()
        os.mkfifo(export / "fifo.json")
        (export / "home").symlink_to(self.root)
        self.assertEqual(orchestrate.untrusted_json(export, ["result.json"], 64), {"exit_code": 0})
        for parts in (["linked.json"], ["big.json"], ["dir.json"], ["fifo.json"], ["home", "secret"],
                      ["..", "secret"], ["/etc/passwd"], ["missing.json"], []):
            with self.assertRaises(orchestrate.Untrusted, msg=parts):
                orchestrate.read_untrusted(export, parts, 64)

    def test_a_result_must_be_well_formed_and_name_a_plain_run(self):
        good = {"exit_code": 0, "run_ids": ["ladder-l1-abc"]}
        self.assertEqual(orchestrate.validated_result(good), {"exit_code": 0, "run_id": "ladder-l1-abc"})
        for bad in ([], {"exit_code": "0", "run_ids": []}, {"exit_code": True, "run_ids": []},
                    {"exit_code": 300, "run_ids": []}, {"exit_code": 0, "run_ids": "x"},
                    {"exit_code": 0, "run_ids": ["../../etc"]}, {"exit_code": 0, "run_ids": ["/abs"]},
                    {"exit_code": 0, "run_ids": ["a,dst=/"]}, {"exit_code": 0, "run_ids": ["a", "b"]}):
            with self.assertRaises(orchestrate.Untrusted, msg=bad):
                orchestrate.validated_result(bad)

    def tar(self, members):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for info, data in members:
                archive.addfile(info, io.BytesIO(data) if data is not None else None)
        stream.seek(0)
        return stream

    def entry(self, name, kind=tarfile.REGTYPE, data=b"", link=""):
        info = tarfile.TarInfo(name)
        info.type, info.linkname = kind, link
        info.size = len(data) if kind == tarfile.REGTYPE else 0
        return info, data if kind == tarfile.REGTYPE else None

    def test_an_export_keeps_only_regular_files_and_directories_inside_it(self):
        stream = self.tar([
            self.entry("./home", tarfile.DIRTYPE),
            self.entry("./home/report.json", data=b"{}"),
            self.entry("./result.json", tarfile.SYMTYPE, link=str(self.secret)),
            self.entry("./home/inner", tarfile.SYMTYPE, link="report.json"),
            self.entry("./hard", tarfile.LNKTYPE, link="./home/report.json"),
            self.entry("/abs.json", data=b"x"),
            self.entry("../escape.json", data=b"x"),
            self.entry("./dev", tarfile.CHRTYPE),
            self.entry("./fifo", tarfile.FIFOTYPE),
        ])
        destination = self.root / "attempt" / "eval"
        skipped = orchestrate.extract_export(stream, destination, 1 << 20)
        found = sorted(str(path.relative_to(destination)) for path in destination.rglob("*"))
        self.assertEqual(found, ["home", "home/report.json"])
        self.assertEqual(len(skipped), 7, skipped)
        self.assertFalse((self.root / "escape.json").exists())
        self.assertEqual(self.secret.read_text(), "host file")

    def test_an_export_is_bounded_in_bytes_and_entries(self):
        big = self.tar([self.entry("./big", data=b"x" * 200_000)])
        with self.assertRaises(orchestrate.Untrusted):
            orchestrate.extract_export(big, self.root / "a" / "eval", 64 * 1024)
        many = self.tar([self.entry(f"./f{index}") for index in range(20)])
        with self.assertRaises(orchestrate.Untrusted):
            orchestrate.extract_export(many, self.root / "b" / "eval", 1 << 20, limit_files=10)


class HostileRun(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.run_dir = pathlib.Path(directory.name) / "run"
        write(self.run_dir / "targets" / "replica-a.json", TARGET)
        source = self.run_dir / "source"
        write(source / "scripts/evals/matrix.json", {"default": ["s1"]})
        fixtures = source / orchestrate.FIXTURES
        for suite in ("s1", "s2"):
            write(fixtures / f"ladder/{suite}/suite.json", {"id": suite, "title": suite, "coverage": []})
            write(fixtures / f"ladder/{suite}/pack_config.json",
                  {"eval_definitions": [{"definition_id": f"{suite}-definition", "cases": ["./cases/a.json"]}]})
            write(fixtures / f"ladder/{suite}/cases/a.json", {"case_id": f"{suite}-case", "split": "train"})
        self.config = {"name": "hostile", "image": "gents-eval:abc", "suites": ["s1"], "splits": ["train"],
                       "trials": 1, "per_replica": 1, "per_trial": 1, "cpuset_pool": "off", "seed_base": 1000,
                       "command_execution": "unrestricted", "replicas": ["replica-a"],
                       "limits": {"cpus": "4", "disk": "1m", "trial_secs": 60}}
        write(self.run_dir / "run.json", self.config)

    def test_a_case_id_that_is_not_a_plain_name_is_refused_before_any_mount(self):
        for case_id in ("a,dst=/etc", "../a", "a b", "a\nb", ""):
            write(self.run_dir / "source" / orchestrate.FIXTURES / "ladder/s1/cases/a.json",
                  {"case_id": case_id, "split": "train"})
            with self.assertRaises(ValueError, msg=case_id):
                orchestrate.Run(self.run_dir).plan()
        self.assertFalse((self.run_dir / "slots").exists())

    def test_names_that_reach_docker_must_be_plain(self):
        for key, value in (("name", "a,b"), ("image", "gents eval"), ("suites", ["s1,dst=/"]),
                           ("limits", {"disk": "1g,uid=0", "trial_secs": 60}), ("command_execution", "yes"),
                           ("cpuset_pool", "4-7 --privileged"), ("cpuset_pool", "7-4"), ("per_trial", 0),
                           ("per_trial", "2")):
            write(self.run_dir / "run.json", {**self.config, key: value})
            with self.assertRaises(ValueError, msg=key):
                orchestrate.Run(self.run_dir)
        with self.assertRaises(ValueError):
            orchestrate.mount_path("/data/runs/a,dst=/etc")

    def test_each_running_attempt_has_its_own_cpu_slice_from_the_pool(self):
        write(self.run_dir / "run.json", {**self.config, "cpuset_pool": "4-13"})
        run = orchestrate.Run(self.run_dir)
        self.assertEqual(run.cpuset_slices(), ["4,5,6,7", "8,9,10,11"])
        run.config["cpuset_pool"] = "auto"
        run.docker = lambda *args: "10\n"
        self.assertEqual(run.cpuset_slices(), ["4,5,6,7"])
        run.docker = lambda *args: "6\n"
        with self.assertRaises(ValueError):
            run.cpuset_slices()
        run.config["cpuset_pool"] = "off"
        self.assertIsNone(run.cpuset_slices())

    def test_a_suite_sees_only_its_own_fixtures(self):
        slots, suites = orchestrate.Run(self.run_dir).plan()
        self.assertEqual(slots, ["s1.s1-case.t0"])
        view = suites["s1"]["view"]
        self.assertTrue((view / "ladder/s1/cases/a.json").is_file())
        self.assertFalse((view / "ladder/s2").exists())

    def done_with(self, slot_id, export):
        write(self.run_dir / "slots" / slot_id / "state.json",
              {"state": "done", "attempt": 1, "run_id": "run-1", "export": str(export)})

    def test_a_hostile_export_never_breaks_the_summary(self):
        run = orchestrate.Run(self.run_dir)
        slots, _ = run.plan()
        export = self.run_dir / "slots" / slots[0] / "attempts" / "1" / "eval"
        report = export / "home/eval/runs/run-1/report.json"
        self.done_with(slots[0], export)
        for hostile in ("not json", json.dumps({"report": {"cells": []}}), json.dumps([1, 2])):
            write(report, hostile)
            summary = orchestrate.summarize(run, slots)
            self.assertEqual(summary["slots"][0]["state"], "unreadable", hostile)
        report.unlink()
        report.symlink_to(self.run_dir / "run.json")
        self.assertEqual(orchestrate.summarize(run, slots)["slots"][0]["state"], "unreadable")
        report.unlink()
        write(report, {"report": {"cells": [{"counts": {"pass": 1}, "usage": {"input_tokens": 5, "output_tokens": 2},
                                             "slots": [{"case_id": "s1-case", "class": "pass", "score_bp": 10000,
                                                        "verdicts": [{"check": "x" * 500}]}]}]}})
        summary = orchestrate.summarize(run, slots)
        entry = summary["slots"][0]
        self.assertEqual((entry["state"], entry["class"], entry["input_tokens"]), ("done", "pass", 5))
        self.assertLessEqual(len(entry["verdicts"][0]["check"]), 130)
        self.assertEqual(summary["results"], "unauthenticated")
        write(self.run_dir / "slots" / slots[0] / "state.json", "{broken")
        self.assertEqual(orchestrate.summarize(run, slots)["slots"][0]["state"], "unreadable")

    def test_settling_an_attempt_trusts_nothing_the_trial_wrote(self):
        run = orchestrate.Run(self.run_dir)
        slots, _ = run.plan()
        (run.dir / "logs" / slots[0]).mkdir(parents=True)
        export = run.dir / "slots" / slots[0] / "attempts" / "1" / "eval"
        export.mkdir(parents=True)
        (export / "result.json").symlink_to(run.dir / "run.json")
        run.settle(slots[0], 1, 0, "exited", export)
        self.assertEqual(run.state(slots[0])["state"], "exit 0")
        (export / "result.json").unlink()
        write(export / "result.json", {"exit_code": 0, "run_ids": ["../../../etc"]})
        run.settle(slots[0], 1, 0, "exited", export)
        self.assertNotEqual(run.state(slots[0])["state"], "done")
        write(export / "result.json", {"exit_code": 0, "run_ids": ["run-1"]})
        run.settle(slots[0], 1, 137, "timed out", export)
        self.assertEqual(run.state(slots[0])["state"], "timed out")
        run.settle(slots[0], 1, 0, "exited", export)
        self.assertEqual(run.state(slots[0])["state"], "done")
        run.settle(slots[0], 2, orchestrate.NOTHING_RAN, "exited", None)
        self.assertEqual(run.state(slots[0])["state"], "exit 3")


if __name__ == "__main__":
    unittest.main()
