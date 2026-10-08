/* Assistant prose, rendered the way the desktop app renders it: GitHub
   markdown, a header on every code block with its language and a copy
   button. Styling comes from the kit's prose-app utility; the block
   header is the one thing added here. */
import { isValidElement, memo, useRef, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { ContribBoundary } from "@/contrib/react/boundary";
import { useContributions } from "@/contrib/react/use-contributions";
import {
  parseTranscriptDirective,
  resolveDirective,
  type ParsedTranscriptDirective,
} from "@/contrib/directives";
import {
  TRANSCRIPT_DIRECTIVE_AREA,
  type TranscriptDirectiveContribution,
} from "@/contrib/types";

function language(children: ReactNode) {
  if (!isValidElement<{ className?: string }>(children)) return null;
  return /language-([\w+-]+)/.exec(children.props.className ?? "")?.[1] ?? null;
}

export function CopyButton({
  getText,
  className,
}: {
  getText: () => string;
  className?: string;
}) {
  const [done, setDone] = useState(false);
  return (
    <Button
      variant="quiet"
      size="icon-xs"
      className={className}
      aria-label="Copy"
      onClick={() => {
        void navigator.clipboard?.writeText(getText());
        setDone(true);
        setTimeout(() => setDone(false), 1200);
      }}
    >
      {done ? <Check /> : <Copy />}
    </Button>
  );
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const pre = useRef<HTMLPreElement>(null);
  const lang = language(children);
  return (
    <div className="relative">
      <div className="absolute top-1 right-1 flex items-center gap-1">
        {lang && (
          <span className="font-mono text-[10px] text-muted-foreground">{lang}</span>
        )}
        <CopyButton getText={() => pre.current?.textContent ?? ""} />
      </div>
      <pre ref={pre}>{children}</pre>
    </div>
  );
}

/* a table that will not fit scrolls on its own rather than squeezing its
   columns into the width it was given */
function Table({ children }: { children?: ReactNode }) {
  return (
    <div className="max-w-full overflow-x-auto">
      <table>{children}</table>
    </div>
  );
}

/* the plain text of a paragraph's children, for the directive check: a
   directive is a bare `::name{...}` line with no inline markup, so anything
   but text nodes means it is not one */
function paragraphText(children: ReactNode): string | null {
  const parts: string[] = [];
  for (const child of Array.isArray(children) ? children : [children]) {
    if (typeof child === "string") parts.push(child);
    else if (child === null || child === undefined || typeof child === "boolean")
      continue;
    else return null;
  }
  return parts.join("");
}

/* a paragraph that is exactly one claimed directive renders as the plugin's
   component, inside its own boundary; everything else is prose. The
   `streaming` flag is unknown here, so a directive is rendered as settled.
   Subscribing to the area means a plugin enabled after the message rendered
   claims its directives without a re-mount. */
function Paragraph({ children }: { children?: ReactNode }) {
  const text = paragraphText(children);
  const directive = text ? parseTranscriptDirective(text) : null;
  const area = useContributions(TRANSCRIPT_DIRECTIVE_AREA);
  const claimed = directive ? resolveDirective(directive.name, area) : null;
  if (!directive || !claimed) return <p>{children}</p>;
  return (
    <ContribBoundary id={`directive:${directive.name}`} variant="chip">
      <DirectiveRender claimed={claimed} directive={directive} />
    </ContribBoundary>
  );
}

function DirectiveRender({
  claimed,
  directive,
}: {
  claimed: TranscriptDirectiveContribution;
  directive: ParsedTranscriptDirective;
}) {
  return (
    <div data-testid="transcript-directive" data-directive={directive.name}>
      {claimed.render({
        attrs: directive.attrs,
        source: directive.source,
        streaming: false,
      })}
    </div>
  );
}

type MdNode = { type: string; value?: string; children?: MdNode[] };

/* a single newline in a paragraph ends the line, as it does in a note a
   person or an agent types, rather than folding into a space; code keeps
   its own newlines because its value is not a text node */
function softBreaks(node: MdNode) {
  if (!node.children) return;
  node.children = node.children.flatMap((child) => {
    if (child.type !== "text" || !child.value?.includes("\n")) {
      softBreaks(child);
      return [child];
    }
    return child.value
      .split("\n")
      .flatMap((value, i) =>
        i === 0
          ? [{ type: "text", value }]
          : [{ type: "break" }, { type: "text", value }],
      );
  });
}
const remarkSoftBreaks = () => softBreaks;

const PLUGINS = [remarkGfm];
const PLUGINS_WITH_BREAKS = [remarkGfm, remarkSoftBreaks];
const COMPONENTS = { pre: CodeBlock, table: Table, p: Paragraph };

export const Markdown = memo(function Markdown({
  children,
  breaks = false,
}: {
  children: string;
  breaks?: boolean;
}) {
  return (
    <ReactMarkdown
      remarkPlugins={breaks ? PLUGINS_WITH_BREAKS : PLUGINS}
      components={COMPONENTS}
    >
      {children}
    </ReactMarkdown>
  );
});
