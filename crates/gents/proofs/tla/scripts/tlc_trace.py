"""Normalize the numbered state messages from TLC 1.7.4's -tool output."""

import json
import re


TOKEN = re.compile(r'\s*(<<|>>|\|->|[][{},]|-?\d+|"(?:\\.|[^"\\])*"|[A-Za-z_]\w*)')


def value(text):
    """The finite TLC values used by replayable configs. Reject any unparsed input."""
    text = text.strip()
    tokens = []
    cursor = 0
    while cursor < len(text):
        match = TOKEN.match(text, cursor)
        if not match:
            raise ValueError(f"unsupported TLC value at {text[cursor:]!r}")
        tokens.append(match[1])
        cursor = match.end()
    tokens.reverse()

    def take(expected):
        if not tokens or tokens.pop() != expected:
            raise ValueError(f"expected {expected!r} in {text!r}")

    def item():
        token = tokens.pop()
        if token in ("TRUE", "FALSE"):
            return token == "TRUE"
        if token.startswith('"') or re.fullmatch(r'-?\d+', token):
            return json.loads(token)
        close = {"<<": ">>", "{": "}", "[": "]"}.get(token)
        if close is None:
            raise ValueError(f"unsupported TLC token {token!r}")
        entries = []
        while tokens and tokens[-1] != close:
            if entries:
                take(",")
            if token == "[":
                key = tokens.pop()
                take("|->")
                entries.append((key, item()))
            else:
                entries.append(item())
        take(close)
        if token == "[":
            return dict(entries)
        if token == "{":
            return sorted(entries, key=lambda v: json.dumps(v, sort_keys=True))
        return entries

    result = item()
    if tokens:
        raise ValueError(f"unparsed TLC value {text!r}")
    return result


def actor(action, before, after):
    """The client identity of the one entry consumed or rebuilt by a quantified step."""
    if action in ("SubmitCommit", "ReserveCommit", "Refuse", "TransportFail"):
        queue = "wire"
    elif action in ("Receive", "LoseReply"):
        queue = "replies"
    else:
        return None
    old, new = before[queue], after[queue]
    candidates = []
    for index, entry in enumerate(old):
        rest = old[:index] + old[index + 1:]
        if rest == new or (len(old) == len(new) and entry != new[index]
                           and rest == new[:index] + new[index + 1:]):
            candidates.append(entry["id"])
    if len(candidates) != 1:
        raise ValueError(f"{action}: ambiguous {queue} transition {old} -> {new}")
    return candidates[0]


def steps(messages):
    result, previous = [], None
    for index, message in enumerate(messages, start=1):
        label, snapshot = message.strip().split("\n", 1)
        match = re.fullmatch(r'(\d+): <(Initial predicate|\w+ line .+)>', label)
        if not match or int(match[1]) != index:
            raise ValueError(f"unexpected TLC state {label!r}")
        fields = re.split(r'(?:^|\n)/\\ (\w+) = ', snapshot)
        if fields[0] or len(fields) < 3:
            raise ValueError(f"malformed TLC state {snapshot!r}")
        state = {key: value(raw.strip()) for key, raw in zip(fields[1::2], fields[2::2])}
        if previous is None:
            if match[2] != "Initial predicate":
                raise ValueError(f"missing initial state: {label!r}")
        else:
            if state.keys() != previous.keys():
                raise ValueError(f"incomplete TLC state {label!r}")
            action = match[2].split()[0]
            result.append(dict(action=action, actor=actor(action, previous, state),
                               before=previous, after=state))
        previous = state
    if not result:
        raise ValueError("missing counterexample transitions")
    return result
