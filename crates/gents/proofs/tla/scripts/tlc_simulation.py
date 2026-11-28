"""Recover action names from TLC 1.7.4's state-only simulation files.

Constrain Next to the sampled edges. TLC labels the resulting state graph
with the action for each edge, including edges visited more than once.
"""
import json
import re
import subprocess
import tempfile
from pathlib import Path

from tla_check import TLA, cfg_sections, expectation, module_files, parse, tlc_command
import tlc_trace


def sample_states(text):
    parts = re.split(r"(?m)^STATE_(\d+) ==\s*\n", text)
    if not re.fullmatch(r"-+ MODULE [^\n]+ -+\n", parts[0]):
        raise ValueError("invalid simulated module header")
    if not re.search(r"(?m)^=+\s*\Z", parts[-1]):
        raise ValueError("unterminated simulated module")
    parts[-1] = re.sub(r"(?m)^=+\s*\Z", "", parts[-1])
    states = []
    for number, body in zip(parts[1::2], parts[2::2]):
        if int(number) != len(states) + 1:
            raise ValueError("nonconsecutive simulation states")
        states.append((body.strip(), tlc_trace.snapshot(body)))
    if len(states) < 2:
        raise ValueError("simulation contains no transition")
    return states


def model_values(value):
    if isinstance(value, dict):
        if "$model" in value:
            yield value["$model"]
        else:
            for entry in value.values():
                yield from model_values(entry)
    if isinstance(value, list):
        for entry in value:
            yield from model_values(entry)


def named_steps(tlc, cfg, directory, samples, bindings, selectors):
    names = sorted(set(model_values(bindings)))
    lines = ["---- MODULE SamplePath ----", f"EXTENDS {cfg.stem}, TLC"]
    if names:
        lines.append("CONSTANTS " + ", ".join(names))
    predicates = [f"SampleState{i}" for i in range(len(samples))]
    for name, (body, _) in zip(predicates, samples):
        lines.append(f"{name} ==\n{body}")
    edges = [f"({a} /\\ {b}')" for a, b in zip(predicates, predicates[1:])]
    lines.extend(["SampleEdge == " + " \\/ ".join(edges), "===="])
    (directory / "SamplePath.tla").write_text("\n".join(lines))
    sections = cfg_sections(cfg.read_text())
    constants = " ".join(sections.get("CONSTANTS", []) + [f"{name} = {name}" for name in names])
    configuration = directory / "SamplePath.cfg"
    configuration.write_text((f"CONSTANTS {constants}\n" if constants else "")
                             + f"INIT {predicates[0]}\nNEXT Next\n"
                             + "ACTION_CONSTRAINT SampleEdge\nCHECK_DEADLOCK FALSE\n")
    scratch = Path(tempfile.mkdtemp(dir=directory))
    graph = scratch / "edges.dot"
    command = tlc_command(tlc.java, tlc.jar, tlc.heap, scratch, configuration, "SamplePath")
    command[-1:-1] = ["-dump", "dot,actionlabels", str(graph)]
    completed = subprocess.run(command, cwd=directory, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, timeout=tlc.timeout)
    verdict = parse(completed.stdout, completed.returncode)
    if verdict.verdict != "holds":
        raise ValueError(f"{cfg.stem} sampled path: {verdict}\n{completed.stdout}")
    return graph_steps(graph.read_text(), [state for _, state in samples], selectors)


def graph_steps(text, samples, selectors):
    quoted = r'("(?:\\.|[^"\\])*")'
    node = re.compile(r'^(-?\d+) \[label=' + quoted)
    edge = re.compile(r'^(-?\d+) -> (-?\d+) \[label=' + quoted)
    states = {}
    operators = {}
    for line in text.splitlines():
        if match := node.match(line):
            state = tlc_trace.snapshot(json.loads(match[2]))
            states[tlc_trace.canonical_key(state)] = match[1]
        elif match := edge.match(line):
            operators.setdefault((match[1], match[2]), set()).add(json.loads(match[3]))
    steps = []
    for before, after in zip(samples, samples[1:]):
        pair = tuple(states[tlc_trace.canonical_key(state)] for state in (before, after))
        actions = operators.get(pair, set())
        if len(actions) != 1:
            raise ValueError(f"sample edge has {len(actions)} possible actions: {actions}")
        action, = actions
        steps.append(dict(action=action, actor=tlc_trace.actor(selectors.get(action), before, after),
                          before=before, after=after))
    return steps


def plans(cfg):
    prefix = r"\* trace sample:"
    offset = 0
    seeds = set()
    for line in cfg.read_text().splitlines():
        if not line.startswith(prefix):
            continue
        match = re.fullmatch(r" seed=(\d+) depth=(\d+) count=(\d+)", line[len(prefix):])
        if not match:
            raise ValueError(f"{cfg.name}: invalid trace sample directive: {line}")
        seed, depth, count = map(int, match.groups())
        if seed in seeds or depth < 2 or count < 1:
            raise ValueError(f"{cfg.name}: invalid or duplicate trace sample: {line}")
        seeds.add(seed)
        yield seed, depth, count, offset
        offset += count


def generate(tlc, spec, cfg, seed, depth, count, offset=0):
    expected = expectation(cfg).verdict
    if expected != "holds":
        raise ValueError(f"{cfg.name}: simulate a holds config")
    bindings = tlc_trace.constants(" ".join(cfg_sections(cfg.read_text())["CONSTANTS"]))
    selectors = tlc_trace.actor_fields((TLA / (spec + ".tla")).read_text())
    results = {}
    with tempfile.TemporaryDirectory(prefix="tla-sample-") as temp:
        directory = Path(temp)
        for module in module_files(cfg):
            (directory / module.name).symlink_to(module)
        command = tlc_command(tlc.java, tlc.jar, tlc.heap, directory, cfg, cfg.stem)
        command.insert(1, "-Dtlc2.tool.Simulator.experimentalLiveness=true")
        command[-1:-1] = ["-simulate", f"num={count},file=sample", "-depth", str(depth), "-seed", str(seed)]
        done = subprocess.run(command, cwd=directory, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True, timeout=tlc.timeout)
        if done.returncode:
            raise ValueError(f"{cfg.name} simulation failed:\n{done.stdout}")
        for i in range(count):
            sampled = sample_states((directory / f"sample_0_{i}").read_text())
            steps = named_steps(tlc, cfg, directory, sampled, bindings, selectors)
            trace = dict(spec=spec, config=cfg.stem, expect=expected, constants=bindings, steps=steps,
                         simulation=dict(seed=seed, depth=depth, sample=i))
            name = f"{cfg.stem}.sample-{offset + i + 1}.json"
            results[name] = (json.dumps(trace, indent=2, sort_keys=True) + "\n").encode()
    return results
