import { isValidElement, memo, useRef, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { useCopied } from "@/lib/clipboard";
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
  const { copied, copy } = useCopied();
  return (
    <Button
      variant="quiet"
      size="icon-xs"
      className={className}
      aria-label="Copy"
      onClick={() => copy(getText())}
    >
      {copied ? <Check /> : <Copy />}
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

/* only a paragraph shaped like a directive subscribes to the area: a long
   reply has hundreds of paragraphs and one toggle would re-render them all */
function Paragraph({ children }: { children?: ReactNode }) {
  const text = paragraphText(children);
  const directive = text ? parseTranscriptDirective(text) : null;
  if (!directive) return <p>{children}</p>;
  return <DirectiveParagraph directive={directive}>{children}</DirectiveParagraph>;
}

/* claimed: the plugin's component inside its own boundary; unclaimed: the
   prose it was. The `streaming` flag is unknown here, so a directive is
   rendered as settled. Subscribing means a plugin enabled after the message
   rendered claims its directives without a re-mount. */
function DirectiveParagraph({
  directive,
  children,
}: {
  directive: ParsedTranscriptDirective;
  children?: ReactNode;
}) {
  const area = useContributions(TRANSCRIPT_DIRECTIVE_AREA);
  const claimed = resolveDirective(directive.name, area);
  if (!claimed) return <p>{children}</p>;
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
const COMPONENTS = { pre: CodeBlock, table: Table };
const COMPONENTS_WITH_DIRECTIVES = { ...COMPONENTS, p: Paragraph };

export const Markdown = memo(function Markdown({
  children,
  breaks = false,
  directives = false,
}: {
  children: string;
  breaks?: boolean;
  /** render `::name{…}` paragraphs through the plugins that claim them.
      Only for the model's own replies in this session: a mailbox body or a
      reasoning trace is not the model addressing this UI, and a plugin
      must not be reachable from text a third party put there. */
  directives?: boolean;
}) {
  return (
    <ReactMarkdown
      remarkPlugins={breaks ? PLUGINS_WITH_BREAKS : PLUGINS}
      components={directives ? COMPONENTS_WITH_DIRECTIVES : COMPONENTS}
    >
      {children}
    </ReactMarkdown>
  );
});
