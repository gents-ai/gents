import { isValidElement, memo, useRef, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { useCopied } from "@/lib/clipboard";

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
