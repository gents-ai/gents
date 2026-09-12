import {
  isValidElement,
  memo,
  useEffect,
  useLayoutEffect,
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
  const shouldAnimate = animate && !prefersReducedMotion();
  const targetLengthRef = useRef(value.length);
  const animationActiveRef = useRef(shouldAnimate);
  const initialVisibleLength = shouldAnimate
    ? Math.min(REVEAL_MIN_CHARS, value.length)
    : value.length;
  const visibleLengthRef = useRef(initialVisibleLength);
  const revealTimerRef = useRef<number | null>(null);
  const [visibleLength, setVisibleLength] = useState(initialVisibleLength);

  const startReveal = () => {
    if (revealTimerRef.current != null) return;
    revealTimerRef.current = window.setTimeout(function tick() {
      revealTimerRef.current = null;
      const current = visibleLengthRef.current;
      const targetLength = targetLengthRef.current;
      if (current >= targetLength) return;

      const remaining = targetLength - current;
      const step = Math.max(
        REVEAL_MIN_CHARS,
        Math.ceil(remaining / REVEAL_MAX_TICKS),
      );
      const next = Math.min(targetLength, current + step);
      visibleLengthRef.current = next;
      setVisibleLength(next);
      if (next < targetLengthRef.current) {
        revealTimerRef.current = window.setTimeout(tick, REVEAL_TICK_MS);
      }
    }, REVEAL_TICK_MS);
  };

  useLayoutEffect(() => {
    targetLengthRef.current = value.length;
    if (!shouldAnimate) {
      if (revealTimerRef.current != null) {
        window.clearTimeout(revealTimerRef.current);
        revealTimerRef.current = null;
      }
      visibleLengthRef.current = value.length;
      setVisibleLength(value.length);
    } else if (!animationActiveRef.current) {
      const next = Math.min(REVEAL_MIN_CHARS, value.length);
      visibleLengthRef.current = next;
      setVisibleLength(next);
      startReveal();
    } else {
      const next = Math.min(visibleLengthRef.current, value.length);
      visibleLengthRef.current = next;
      setVisibleLength(next);
      if (next < value.length) startReveal();
    }
    animationActiveRef.current = shouldAnimate;
  }, [shouldAnimate, value]);

  useEffect(() => {
    return () => {
      if (revealTimerRef.current != null) {
        window.clearTimeout(revealTimerRef.current);
        revealTimerRef.current = null;
      }
    };
  }, []);

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
