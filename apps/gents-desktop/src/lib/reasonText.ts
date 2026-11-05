/** Words for what the runtime and providers report: statuses, causes and
    error strings become sentences a person reads. The raw text is kept for
    a notice's Details, never shown as the message itself, and a command a
    person would have to type elsewhere is never offered as the way out. */

/** Trimmed, with whitespace collapsed, a capital first letter and a full
    stop, so a fragment reads as a sentence. */
export function sentence(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (!flat) return "";
  const capital = flat[0]!.toUpperCase() + flat.slice(1);
  return /[.!?…)]$/.test(capital) ? capital : `${capital}.`;
}

/** `text` without its clauses that tell the person to run a command, such
    as "; add one with `gents auth login claude --label <label>`". */
export function withoutCommandHints(text: string): string {
  return text
    .replace(/[;,.]?\s*[^;,.]*`[^`]*`[^;,.]*/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

/* "code: message" as the runtime writes many reasons */
const CODED = /^([a-z][a-z0-9]*(?:_[a-z0-9]+)+|[a-z]+Error|[a-z]+):\s+(.+)$/s;

const CODES: Record<string, string> = {
  verification_failed: "The check of the work didn't pass",
  usage_limited: "The account reached its usage limit",
  budget_limited: "The goal used up its token budget",
  provider_error: "The provider returned an error",
  tool_error: "A tool failed",
  timeout: "It took too long",
};

/** A reported reason, as a sentence: a known code's words, else the
    message after any "code:" prefix, without command hints. */
export function reasonText(raw: string | null | undefined): string {
  const text = withoutCommandHints((raw ?? "").trim());
  if (!text) return "";
  const coded = CODED.exec(text);
  if (coded) {
    const [, code, rest] = coded;
    const known = CODES[code!];
    if (known) {
      const said = sentence(withoutCommandHints(rest!));
      return `${known}: ${said.charAt(0).toLowerCase()}${said.slice(1)}`;
    }
    return sentence(rest!);
  }
  return sentence(text);
}

const FAILURES: [RegExp, string][] = [
  [/overloaded|\b529\b/i, "The provider was overloaded. Try again in a moment."],
  [/usage limit/i, "The account reached its usage limit."],
  [
    /rate.?limit|\b429\b/i,
    "The provider is limiting how often it can be asked. Try again shortly.",
  ],
  [
    /context (?:length|window)|too long for|maximum context/i,
    "The conversation is too long for this model.",
  ],
  [
    /\b401\b|\b403\b|unauthori[sz]ed|forbidden|invalid.{0,20}(?:key|token)|credentials/i,
    "The provider didn't accept the account's credentials.",
  ],
  [/timed? ?out|deadline|took too long/i, "The provider took too long to answer."],
  [
    /connection refused|econnrefused|network|dns|unreachable|couldn['’]t connect|os error 6[01]/i,
    "The provider couldn't be reached.",
  ],
];

/** Why a turn or a request failed, in words, or null when the error isn't
    one we recognise. */
export function failureText(raw: string | null | undefined): string | null {
  const text = raw ?? "";
  for (const [pattern, words] of FAILURES) if (pattern.test(text)) return words;
  return null;
}

const UNREACHABLE =
  /connection refused|econnrefused|unreachable|couldn['’]t connect|os error 6[01]/i;

/** An error from the app's own calls to show the person: a node that can't
    be reached, a recognised failure's words, else the error itself as a
    sentence without command hints. */
export function errorWords(raw: string | null | undefined): string {
  if (UNREACHABLE.test(raw ?? ""))
    return "The node couldn't be reached. Check that it's running.";
  return failureText(raw) ?? reasonText(raw);
}

const GOAL_STATUS: Record<string, string> = {
  active: "Working",
  paused: "Paused",
  blocked: "Blocked",
  usage_limited: "Stopped at the account's usage limit",
  budget_limited: "Stopped at its token budget",
  complete: "Met",
};

/** A goal's status as words. */
export function goalStatusText(status: string | null | undefined): string {
  if (!status) return "";
  return GOAL_STATUS[status] ?? sentence(status.replace(/_/g, " ")).replace(/\.$/, "");
}

const CANCEL_CAUSE: Record<string, string> = {
  userCancelled: "You stopped it",
  deadline: "It reached its deadline",
  interrupted: "It was interrupted",
};

/** Why something was stopped, as a clause: "You stopped it". */
export function cancelCauseText(cause: string | null | undefined): string {
  return (cause && CANCEL_CAUSE[cause]) || "It was stopped";
}

const STOP_SOURCE: Record<string, string> = {
  requestInterrupt: "a stop request on this request",
  requestLifecycle: "the request's lifecycle state",
  deadline: "its deadline",
};

/** What reported a stop, in words. */
export function stopSourceText(source: string): string {
  return STOP_SOURCE[source] ?? source;
}
