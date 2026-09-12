import {
  isValidElement,
  memo,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
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

const REVEAL_TICK_MS = 32;
const REVEAL_MAX_TICKS = 12;
const REVEAL_MIN_CHARS = 16;

function prefersReducedMotion() {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * Smooth a newly observed response burst without delaying ordinary token
 * streaming or replaying animation for transcript history.
 */
export function RevealedMarkdownContent({
  value,
  animate,
}: {
  value: string;
  animate: boolean;
}) {
  const animateRef = useRef(animate && !prefersReducedMotion());
  if (animate) animateRef.current = !prefersReducedMotion();
  const [visibleLength, setVisibleLength] = useState(() =>
    animateRef.current
      ? Math.min(REVEAL_MIN_CHARS, value.length)
      : value.length,
  );

  useEffect(() => {
    if (!animateRef.current) {
      setVisibleLength(value.length);
      return;
    }
    if (visibleLength >= value.length) return;

    const remaining = value.length - visibleLength;
    const step = Math.max(
      REVEAL_MIN_CHARS,
      Math.ceil(remaining / REVEAL_MAX_TICKS),
    );
    const timer = window.setTimeout(
      () =>
        setVisibleLength((current) => Math.min(value.length, current + step)),
      REVEAL_TICK_MS,
    );
    return () => window.clearTimeout(timer);
  }, [value, visibleLength]);

  return (
    <div aria-busy={visibleLength < value.length ? "true" : undefined}>
      <MarkdownContent value={value.slice(0, visibleLength)} />
    </div>
  );
}

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
