import { isValidElement, memo, useRef, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

import { CopyButton, formatMessageTime } from "@source-inc/gents-desktop-ui";

function codeBlockLanguage(children: ReactNode): string | null {
  if (!isValidElement<{ className?: string }>(children)) {
    return null;
  }
  const match = /language-([\w+-]+)/.exec(children.props.className ?? "");
  return match ? match[1] : null;
}

export function CodeBlock(props: { children?: ReactNode }) {
  const preRef = useRef<HTMLPreElement | null>(null);
  const language = codeBlockLanguage(props.children);
  return (
    <div className="code-block">
      <div className="code-block-header">
        {language ? (
          <span className="code-block-language">{language}</span>
        ) : null}
        <CopyButton
          className="code-block-copy"
          getText={() => preRef.current?.textContent ?? ""}
        />
      </div>
      <pre ref={preRef}>{props.children}</pre>
    </div>
  );
}

export const MarkdownContent = memo(function MarkdownContent({
  value,
}: {
  value: string;
}) {
  return (
    <div className="markdown-content">
      <ReactMarkdown
        components={{ pre: CodeBlock }}
        rehypePlugins={[rehypeHighlight]}
        remarkPlugins={[remarkGfm]}
      >
        {value}
      </ReactMarkdown>
    </div>
  );
});

export function normalizeTranscriptText(value?: string | null) {
  return value?.trim() ?? "";
}

export function ReasoningDisclosure({
  value,
  summary = "Thinking",
}: {
  value?: string | null;
  summary?: string;
}) {
  const normalized = normalizeTranscriptText(value);
  if (!normalized) {
    return null;
  }

  return (
    <details className="reasoning-disclosure">
      <summary className="reasoning-summary">{summary}</summary>
      <div className="message-reasoning">
        <MarkdownContent value={normalized} />
      </div>
    </details>
  );
}

export function MessageTime({ value }: { value?: string | null }) {
  const label = formatMessageTime(value);
  if (!label) {
    return null;
  }
  return (
    <time
      className="message-time"
      dateTime={value ?? undefined}
      title={value ?? undefined}
    >
      {label}
    </time>
  );
}
