/* Assistant prose, rendered the way the desktop app renders it: GitHub
   markdown, a header on every code block with its language and a copy
   button. Styling comes from the kit's prose-app utility; the block
   header is the one thing added here. */
import { isValidElement, useRef, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { Button } from "@gents/ui/components/button";

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

export function Markdown({ children }: { children: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ pre: CodeBlock }}>
      {children}
    </ReactMarkdown>
  );
}
