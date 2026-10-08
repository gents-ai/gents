/* Transcript directives: the transcript as a contribution area. A plugin
   registers a named directive; the model addresses it by emitting a
   paragraph `::name{key="value"}`, and that paragraph renders as the
   plugin's component inline in the assistant message. Nothing renders
   unless a plugin claimed the name: an unclaimed or malformed directive
   stays the prose it always was. Attributes are untrusted model output;
   a plugin validates its own fields. After hermes-agent's
   lib/transcript-directives.ts (the whole-paragraph form only). */
import { registry } from "./registry";
import {
  TRANSCRIPT_DIRECTIVE_AREA,
  type Contribution,
  type TranscriptDirectiveContribution,
} from "./types";

export interface ParsedTranscriptDirective {
  name: string;
  attrs: Record<string, string>;
  source: string;
}

/* the whole paragraph, nothing else: `::name` or `::name{...}`; the length
   caps bound the attribute scan on adversarial input */
const DIRECTIVE_RE = /^::([a-z][a-z0-9-]{0,63})(?:\{([^{}]{0,1024})\})?$/;

/* key="value" pairs; single quotes accepted for model sloppiness */
const ATTR_RE = /([a-z][\w-]{0,63})=(?:"([^"]*)"|'([^']*)')/gi;

/** a directive name a manifest may declare */
export const DIRECTIVE_NAME_RE = /^[a-z][a-z0-9-]{0,63}$/;

/* null unless the entire trimmed text is one directive; pure, so it is
   safe during render */
export function parseTranscriptDirective(
  text: string,
): ParsedTranscriptDirective | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("::") || trimmed.length > 1200 || trimmed.includes("\n")) {
    return null;
  }
  const match = DIRECTIVE_RE.exec(trimmed);
  if (!match) return null;
  const attrs: Record<string, string> = {};
  if (match[2]) {
    for (const pair of match[2].matchAll(ATTR_RE)) {
      attrs[pair[1]!.toLowerCase()] = pair[2] ?? pair[3] ?? "";
    }
  }
  return { name: match[1]!, attrs, source: trimmed };
}

/* the contribution that claimed a name; first registration wins, so a
   later plugin cannot hijack a name an earlier one owns. Takes the area
   snapshot so a React caller can pass what it subscribed to. */
export function resolveDirective(
  name: string,
  area: readonly Contribution[] = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA),
): TranscriptDirectiveContribution | null {
  for (const c of area) {
    const data = c.data as TranscriptDirectiveContribution | undefined;
    if (data?.name === name && typeof data.render === "function") return data;
  }
  return null;
}
