"""Normalize the finite values and numbered states emitted by the pinned TLC."""

import json
import re
from typing import NamedTuple


TOKEN = re.compile(r'\s*(<<|>>|\|->|:>|@@|[][(){},=]|-?\d+|"(?:\\.|[^"\\])*"|[A-Za-z_]\w*)')
NAME = re.compile(r'[A-Za-z_]\w*')
LOCATION = r'(\w+) line \d+, col \d+ to line \d+, col \d+ of module \w+'


def canonical_key(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


class Values:
    def __init__(self, text):
        self.tokens = []
        pos = 0
        while pos < len(text):
            match = TOKEN.match(text, pos)
            if not match:
                raise ValueError(f"unsupported TLC value at {text[pos:]!r}")
            self.tokens.append(match[1])
            pos = match.end()
        self.pos = 0

    def peek(self):
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def take(self, expected=None):
        token = self.peek()
        if token is None or (expected is not None and token != expected):
            raise ValueError(f"expected {expected or 'a value'}, got {token!r}")
        self.pos += 1
        return token

    def name(self):
        name = self.take()
        if not NAME.fullmatch(name):
            raise ValueError(f"expected an identifier, got {name}")
        return name

    def read(self):
        token = self.take()
        if token in ("TRUE", "FALSE"):
            return token == "TRUE"
        if token.startswith('"') or re.fullmatch(r'-?\d+', token):
            return json.loads(token)
        if NAME.fullmatch(token):
            return {"$model": token}
        if token == "(":
            entries = []
            keys = set()
            while True:
                key = self.read()
                self.take(":>")
                identity = canonical_key(key)
                if identity in keys:
                    raise ValueError(f"duplicate function key {key}")
                keys.add(identity)
                entries.append([key, self.read()])
                if self.peek() != "@@":
                    break
                self.take("@@")
            self.take(")")
            return {"$function": sorted(entries, key=lambda entry: canonical_key(entry[0]))}
        close = {"<<": ">>", "{": "}", "[": "]"}.get(token)
        if close is None:
            raise ValueError(f"unexpected TLC token {token!r}")
        entries = []
        while self.peek() != close:
            if entries:
                self.take(",")
            if token == "[":
                key = self.name()
                self.take("|->")
                entries.append((key, self.read()))
            else:
                entries.append(self.read())
        self.take(close)
        if token == "[":
            result = dict(entries)
            if len(result) != len(entries):
                raise ValueError("duplicate record field")
            return result
        if token == "{":
            return sorted(entries, key=canonical_key)
        return entries


def value(text):
    values = Values(text.strip())
    result = values.read()
    if values.peek() is not None:
        raise ValueError(f"unparsed TLC value {text!r}")
    return result


def constants(text):
    values = Values(text.strip())
    result = {}
    while values.peek() is not None:
        name = values.name()
        if name in result:
            raise ValueError(f"duplicate constant {name}")
        values.take("=")
        result[name] = values.read()
    return result


class ActorSource(NamedTuple):
    field: str | None
    count: int


def actor_fields(source):
    fields = {}
    for line in source.splitlines():
        prefix = r"\* trace actors: "
        if not line.startswith(prefix):
            continue
        actions, selector = line[len(prefix):].split(" => ")
        if not re.fullmatch(r'\w+(?:(?:\|\w+)+|\.\w+|\{(?:member|2)\})?', selector):
            raise ValueError(f"invalid trace actor selector {selector}")
        for action in actions.split(", "):
            if not NAME.fullmatch(action) or action in fields:
                raise ValueError(f"invalid or duplicate trace action {action}")
            fields[action] = ActorSource(selector.removesuffix("{2}"), 2 if selector.endswith("{2}") else 1)
    for name, params in re.findall(r"(?m)^(\w+)\(([\w, ]+)\)\s*==", source):
        fields.setdefault(name, ActorSource(None, len(params.split(","))))
    return fields


def changed_keys(old, new):
    def entries(data):
        if isinstance(data, list):
            return list(enumerate(data, 1))
        if isinstance(data, dict):
            return data["$function"] if "$function" in data else sorted(data.items())
        raise ValueError(f"actor field has no indexed domain: {data}")
    left = {canonical_key(k): (k, v) for k, v in entries(old)}
    right = {canonical_key(k): (k, v) for k, v in entries(new)}
    return [(left[k] if k in left else right[k])[0] for k in sorted(left.keys() | right.keys())
            if left.get(k) != right.get(k)]


def actor(source, before, after):
    if source is None:
        return None
    if source.field is None:
        if source.count != 1:
            raise ValueError("multiple action parameters need an explicit symmetric actor source")
        changed = [changed_keys(old, after[field]) for field, old in before.items()
                   if isinstance(old, dict) and "$function" in old and old != after[field]]
        groups = [keys for keys in changed if len(keys) == source.count]
        if not groups or any(keys != groups[0] for keys in groups):
            raise ValueError("ambiguous parameterized action: declare its trace actors")
        candidates = groups[0]
    else:
        field, _, identity = source.field.partition(".")
        if field.endswith("{member}"):
            name = field.removesuffix("{member}")
            old = {canonical_key(v): v for v in before[name]}
            new = {canonical_key(v): v for v in after[name]}
            candidates = [(old[k] if k in old else new[k]) for k in sorted(old.keys() ^ new.keys())]
        elif identity:
            old, new = before[field], after[field]
            candidates = []
            for i, entry in enumerate(old):
                rest = old[:i] + old[i + 1:]
                if rest == new or (len(old) == len(new) and entry != new[i]
                                   and rest == new[:i] + new[i + 1:]):
                    candidates.append(entry[identity])
        else:
            changes = {canonical_key(k): k for name in field.split("|")
                       for k in changed_keys(before[name], after[name])}
            candidates = [changes[k] for k in sorted(changes)]
    if len(candidates) != source.count:
        raise ValueError(f"{source}: expected {source.count} changed actors, got {candidates}")
    return candidates[0] if source.count == 1 else candidates


def snapshot(text):
    fields = re.split(r'(?:^|\n)(?:/\\ )?(\w+) = ', text.strip())
    if fields[0] or len(fields) < 3:
        raise ValueError(f"malformed TLC state {text!r}")
    state = {key: value(raw) for key, raw in zip(fields[1::2], fields[2::2])}
    if len(state) != len(fields[1::2]):
        raise ValueError("duplicate state variable")
    return state


def steps(messages, selectors):
    states, result, loop = [], [], None
    for code, message in messages:
        label, _, body = message.strip().partition("\n")
        if loop is not None:
            raise ValueError("unexpected state after the end of a lasso")
        if code == "2217":
            match = re.fullmatch(rf'(\d+): <(?:Initial predicate|{LOCATION})>', label)
            if not match or int(match[1]) != len(states) + 1:
                raise ValueError(f"unexpected TLC state {label!r}")
            action, state = match[2], snapshot(body)
            if bool(states) != bool(action):
                raise ValueError(f"unexpected initial predicate: {label!r}")
        elif code == "2218" and states and label == f"{len(states) + 1}: Stuttering" and not body:
            action, state, loop = "Stutter", states[-1], len(states)
        elif code == "2122" and states and not body:
            match = re.fullmatch(rf'(\d+): Back to state: <{LOCATION}>', label)
            if not match or not 1 <= int(match[1]) <= len(states):
                raise ValueError(f"invalid back edge {label!r}")
            loop, action = int(match[1]), match[2]
            state = states[loop - 1]
        else:
            raise ValueError(f"unsupported TLC trace message {code}: {label!r}")
        if states:
            before = states[-1]
            if state.keys() != before.keys():
                raise ValueError(f"incomplete TLC state {label!r}")
            step = dict(action=action, actor=actor(selectors.get(action), before, state),
                        before=before, after=state)
            if loop is not None:
                step["backToState"] = loop
            result.append(step)
        states.append(state)
    if not result:
        raise ValueError("missing counterexample transitions")
    return result
