import { isValidElement, useRef, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

import { CopyButton } from "./CopyButton";

import { parseCommandDenial } from "../lib/commandDenial";
import type {
  RenderedTimelineItem,
  RenderedToolCallView,
  ToolDetailValueView,
} from "../lib/types";
import type { DerivedCancelCauseView } from "../lib/types/operations";
import { CancelCauseBadge, CancelCauseDetails } from "./cancelUx";
import { CodeToolItem } from "./codeTools/CodeToolItem";
import { toCodeToolView } from "./codeTools/codeTools";
import { CommandDenialToolItem } from "./commandDenial";

function codeBlockLanguage(children: ReactNode): string | null {
  if (!isValidElement<{ className?: string }>(children)) {
    return null;
  }
  const match = /language-([\w+-]+)/.exec(children.props.className ?? "");
  return match ? match[1] : null;
}

function CodeBlock(props: { children?: ReactNode }) {
  const preRef = useRef<HTMLPreElement | null>(null);
  const language = codeBlockLanguage(props.children);
  return (
    <div className="code-block">
      <div className="code-block-header">
        {language ? <span className="code-block-language">{language}</span> : null}
        <CopyButton
          className="code-block-copy"
          getText={() => preRef.current?.textContent ?? ""}
        />
      </div>
      <pre ref={preRef}>{props.children}</pre>
    </div>
  );
}

function MarkdownContent({ value }: { value: string }) {
  return (
    <div className="markdown-content">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{ pre: CodeBlock }}
      >
        {value}
      </ReactMarkdown>
    </div>
  );
}

function normalizeTranscriptText(value?: string | null) {
  if (!value) {
    return "";
  }

  return value.trim();
}

function toolStatusClass(statusKind?: string | null) {
  switch ((statusKind ?? "").toLowerCase()) {
    case "success":
      return "tool-item-dot tool-item-dot-success";
    case "error":
      return "tool-item-dot tool-item-dot-error";
    default:
      return "tool-item-dot tool-item-dot-running";
  }
}

function ToolDetailSection({
  label,
  value,
}: {
  label: string;
  value?: ToolDetailValueView | null;
}) {
  if (!value?.rawText.trim()) {
    return null;
  }

  return (
    <div className="tool-detail">
      <div className="tool-detail-label">{label}</div>
      {value.fields.length > 0 ? (
        <div className="tool-detail-grid">
          {value.fields.map((field) => (
            <div className="tool-detail-row" key={field.key}>
              <div className="tool-detail-key">{field.key}</div>
              <div className="tool-detail-value">{field.value}</div>
            </div>
          ))}
        </div>
      ) : (
        <pre className="tool-block">{value.rawText}</pre>
      )}
    </div>
  );
}

/** One-line digest of a call's arguments for the collapsed summary. */
function toolArgsPreview(args?: ToolDetailValueView | null): string | null {
  if (!args) {
    return null;
  }
  const source = args.fields.find((field) => field.value.trim())?.value ?? args.rawText;
  const flat = source.replace(/\s+/g, " ").trim();
  if (!flat) {
    return null;
  }
  return flat.length > 64 ? `${flat.slice(0, 64)}…` : flat;
}

function ToolGroups({ tools }: { tools: RenderedToolCallView[] }) {
  return (
    <section className="tool-group">
      {tools.map((tool) => {
        // Prefer structured DenialReason fields persisted on AgentToolCall.
        // The parser remains as a compatibility fallback for older rows.
        const denial =
          (tool.statusKind ?? "").toLowerCase() === "error"
            ? (tool.denial ?? parseCommandDenial(tool.result?.rawText))
            : null;
        if (denial) {
          return (
            <CommandDenialToolItem key={tool.itemKey} tool={tool} denial={denial} />
          );
        }
        // Code-aware rendering: file edits become diffs, bash becomes a terminal
        // block. Only completed, uncancelled calls are projected — errored,
        // running, and cancelled calls keep the generic disclosure (with its
        // cancel-cause details), so a failed edit never reads as an applied diff.
        // A completed bash with a non-zero/timeout envelope still projects; the
        // envelope drives its failed badge.
        const codeView =
          (tool.statusKind ?? "").toLowerCase() === "success" && !tool.cancelCause
            ? toCodeToolView(tool)
            : null;
        if (codeView) {
          return <CodeToolItem key={tool.itemKey} view={codeView} />;
        }
        return (
          <details className="tool-item" key={tool.itemKey}>
            <summary className="tool-item-summary">
              <span className="tool-item-summary-left">
                <span aria-hidden="true" className={toolStatusClass(tool.statusKind)} />
                <span className="tool-item-name">{tool.toolName}</span>
                {toolArgsPreview(tool.args) ? (
                  <span className="tool-item-preview">
                    {toolArgsPreview(tool.args)}
                  </span>
                ) : null}
                {tool.cancelCause ? (
                  <CancelCauseBadge
                    cause={tool.cancelCause}
                    className="tool-item-cause-badge"
                  />
                ) : null}
              </span>
              <span aria-hidden="true" className="tool-item-action">
                ▸
              </span>
            </summary>
            <div className="tool-item-body">
              {tool.cancelCause ? (
                <CancelCauseDetails cause={tool.cancelCause} />
              ) : null}
              <ToolDetailSection label="args" value={tool.args} />
              <ToolDetailSection label="result" value={tool.result} />
            </div>
          </details>
        );
      })}
    </section>
  );
}

function ReasoningDisclosure({
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

function hasVisibleResponseCancelBadgeTarget(
  item: RenderedTimelineItem,
  responseMaterializedSequence?: number | null,
) {
  switch (item.kind) {
    case "assistantMessage":
      return (
        Boolean(
          normalizeTranscriptText(item.content) ||
          normalizeTranscriptText(item.reasoning),
        ) &&
        item.sequence != null &&
        item.sequence === responseMaterializedSequence
      );
    case "liveAssistant":
      return Boolean(
        normalizeTranscriptText(item.content) ||
        normalizeTranscriptText(item.reasoning),
      );
    default:
      return false;
  }
}

function AssistantCancelCauseTurn({ cause }: { cause: DerivedCancelCauseView }) {
  return (
    <div className="turn-block">
      <article className="message-card">
        <div className="message-role">
          assistant
          <CancelCauseBadge cause={cause} className="assistant-turn-cause-badge" />
        </div>
        <CancelCauseDetails cause={cause} />
      </article>
    </div>
  );
}

export function MessageList({
  timelineItems,
  responseCancelCause,
  responseMaterializedSequence,
}: {
  timelineItems: RenderedTimelineItem[];
  responseCancelCause?: DerivedCancelCauseView | null;
  responseMaterializedSequence?: number | null;
}) {
  const shouldRenderStandaloneCancelCause =
    responseCancelCause != null &&
    !timelineItems.some((item) =>
      hasVisibleResponseCancelBadgeTarget(item, responseMaterializedSequence),
    );

  return (
    <>
      {timelineItems.map((item, index) => {
        const timelineKey = `${item.kind}-${item.itemKey}-${index}`;
        switch (item.kind) {
          case "userMessage":
            return (
              <div className="turn-block" key={timelineKey}>
                <article className="message-card user-card">
                  <div className="message-role">
                    user
                    <CopyButton
                      className="message-copy"
                      getText={() => normalizeTranscriptText(item.content)}
                    />
                  </div>
                  <div className="message-content">
                    <MarkdownContent value={normalizeTranscriptText(item.content)} />
                  </div>
                </article>
              </div>
            );
          case "assistantMessage": {
            const normalizedContent = normalizeTranscriptText(item.content);
            const normalizedReasoning = normalizeTranscriptText(item.reasoning);
            if (!normalizedContent && !normalizedReasoning) {
              return null;
            }
            const showBadge =
              responseCancelCause != null &&
              item.sequence != null &&
              item.sequence === responseMaterializedSequence;
            return (
              <div className="turn-block" key={timelineKey}>
                <article className="message-card">
                  <div className="message-role">
                    assistant
                    {showBadge ? (
                      <CancelCauseBadge
                        cause={responseCancelCause}
                        className="assistant-turn-cause-badge"
                      />
                    ) : null}
                    {normalizedContent ? (
                      <CopyButton
                        className="message-copy"
                        getText={() => normalizedContent}
                      />
                    ) : null}
                  </div>
                  {normalizedContent ? (
                    <div className="message-content">
                      <MarkdownContent value={normalizedContent} />
                    </div>
                  ) : null}
                  <ReasoningDisclosure value={normalizedReasoning} />
                </article>
              </div>
            );
          }
          case "toolGroup":
            return (
              <div className="turn-block" key={timelineKey}>
                <ToolGroups tools={item.tools} />
              </div>
            );
          case "pendingUserTurn":
            return (
              <div className="turn-block" key={timelineKey}>
                <article className="message-card pending-card">
                  <div className="message-role">user</div>
                  <div className="message-content">
                    <MarkdownContent value={normalizeTranscriptText(item.content)} />
                  </div>
                </article>
              </div>
            );
          case "liveAssistant": {
            const overlayContent = normalizeTranscriptText(item.content);
            const overlayReasoning = normalizeTranscriptText(item.reasoning);
            if (!overlayContent && !overlayReasoning) {
              return null;
            }
            return (
              <article className="message-card" key={timelineKey}>
                <div className="message-role">
                  assistant
                  {responseCancelCause != null ? (
                    <CancelCauseBadge
                      cause={responseCancelCause}
                      className="assistant-turn-cause-badge"
                    />
                  ) : null}
                </div>
                {overlayContent ? (
                  <div className="message-content">
                    <MarkdownContent value={overlayContent} />
                  </div>
                ) : null}
                <ReasoningDisclosure value={overlayReasoning} />
              </article>
            );
          }
          default:
            return null;
        }
      })}
      {shouldRenderStandaloneCancelCause ? (
        <AssistantCancelCauseTurn cause={responseCancelCause} />
      ) : null}
    </>
  );
}
