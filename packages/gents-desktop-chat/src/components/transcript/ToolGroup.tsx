import type {
  RenderedToolCallView,
  ToolDiffLineKind,
  ToolPresentationView,
} from "@source-inc/gents-desktop-client";
import { CopyButton } from "@source-inc/gents-desktop-ui";
import { memo, useContext, useState } from "react";
import { TranscriptNavigationContext } from "./TranscriptNavigation.js";
import {
  actionCommand,
  actionEntries,
  actionKind,
  actionItemKeys,
  actionStatus,
  actionTitle,
  type ActionEntry,
} from "./actionPresentation.js";

import { CancelCauseBadge, CancelCauseDetails } from "../cancelUx/index.js";
import { CommandDenialToolItem } from "../commandDenial/index.js";

const DIFF_MARK: Record<ToolDiffLineKind, string> = {
  added: "+",
  removed: "-",
  context: " ",
};

function statusClass(statusKind: string) {
  switch (statusKind.toLowerCase()) {
    case "success":
      return "tool-item-dot tool-item-dot-success";
    case "error":
      return "tool-item-dot tool-item-dot-error";
    case "cancelled":
      return "tool-item-dot tool-item-dot-cancelled";
    default:
      return "tool-item-dot tool-item-dot-running";
  }
}

function statusLabel(tool: RenderedToolCallView) {
  switch (actionStatus(tool)) {
    case "success":
      return "completed";
    case "error":
      return "failed";
    case "cancelled":
      return "stopped";
    case "unknown":
      return "unknown";
    default:
      return "working";
  }
}

function compact(value: string | null | undefined, maxLength = 80) {
  const flat = value?.replace(/\s+/g, " ").trim();
  if (!flat) return null;
  return flat.length > maxLength ? `${flat.slice(0, maxLength)}…` : flat;
}

function formatPayload(value: string) {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
}

const Payload = memo(function Payload({
  label,
  value,
}: {
  label: string;
  value?: string | null;
}) {
  if (!value?.trim()) return null;
  const formatted = formatPayload(value);
  return (
    <div className="tool-payload-section">
      <div className="tool-detail-label">{label}</div>
      <div className="tool-payload-wrap">
        <CopyButton className="tool-payload-copy" getText={() => formatted} />
        <pre className="tool-payload">{formatted}</pre>
      </div>
    </div>
  );
});

function LiveOutput({ tool }: { tool: RenderedToolCallView }) {
  const tail =
    tool.statusKind.toLowerCase() === "running"
      ? (tool.partialOutputTail ?? null)
      : null;
  if (!tail) return null;
  return (
    <div className="tool-live-tail" data-testid={`tool-live-${tool.itemKey}`}>
      <span className="tool-live-tail-label">
        live output
        <span aria-hidden="true" className="tool-live-dot" />
      </span>
      <pre>{tail}</pre>
    </div>
  );
}

function ToolPayloadAvailability({ tool }: { tool: RenderedToolCallView }) {
  switch (tool.reconstruction.state) {
    case "ready":
      return null;
    case "loading":
      if (tool.partialOutputTail) return null;
      return (
        <p
          className="tool-output-notice"
          role="status"
          data-testid={`tool-output-loading-${tool.itemKey}`}
        >
          {tool.statusKind.toLowerCase() === "running"
            ? "Waiting for output"
            : "Loading output"}
        </p>
      );
    case "denied":
      return (
        <p role="alert" data-testid={`tool-output-denied-${tool.itemKey}`}>
          Tool output unavailable: access denied.
        </p>
      );
    case "invalid":
      return (
        <p role="alert" data-testid={`tool-output-invalid-${tool.itemKey}`}>
          Tool output could not be reconstructed.
        </p>
      );
  }
}

function commonBadges(tool: RenderedToolCallView) {
  return (
    <>
      {tool.awaitMode ? (
        <span className="tool-item-mode">{tool.awaitMode}</span>
      ) : null}
      <span
        className={`tool-item-status tool-item-status-${actionStatus(tool)}`}
      >
        {statusLabel(tool)}
      </span>
      {tool.cancelCause ? (
        <CancelCauseBadge
          cause={tool.cancelCause}
          className="tool-item-cause-badge"
        />
      ) : null}
    </>
  );
}

function commandExit(
  presentation: Extract<ToolPresentationView, { kind: "command" }>,
) {
  if (presentation.timedOut) return "timed out";
  if (presentation.exitCode != null) return `exit ${presentation.exitCode}`;
  if (presentation.failed) return "failed";
  return null;
}

function readCount(
  presentation: Extract<ToolPresentationView, { kind: "fileRead" }>,
) {
  if (presentation.returnedCount == null) return null;
  const total =
    presentation.totalCount != null &&
    presentation.totalCount !== presentation.returnedCount
      ? ` of ${presentation.totalCount}`
      : "";
  return `${presentation.returnedCount}${total}${presentation.truncated ? " · truncated" : ""}`;
}

function ToolSummary({ tool }: { tool: RenderedToolCallView }) {
  if (tool.reconstruction.state !== "ready") {
    return (
      <>
        <span className="tool-primary mono">{tool.toolName}</span>
        {commonBadges(tool)}
      </>
    );
  }
  const view = tool.presentation;
  if (view.kind === "command") {
    const exit = actionStatus(tool) === "cancelled" ? null : commandExit(view);
    return (
      <>
        <span aria-hidden="true" className="tool-command-prompt mono">
          $
        </span>
        <span className="tool-primary mono">{view.command}</span>
        {commonBadges(tool)}
        {exit ? (
          <span className={view.failed ? "tool-exit is-error" : "tool-exit"}>
            {exit}
          </span>
        ) : null}
      </>
    );
  }
  if (view.kind === "fileRead") {
    return (
      <>
        <span className="tool-kind">{view.operation.replace("_file", "")}</span>
        <span className="tool-primary mono">
          {view.target ?? tool.toolName}
        </span>
        {readCount(view) ? (
          <span className="tool-secondary">{readCount(view)}</span>
        ) : null}
        {commonBadges(tool)}
      </>
    );
  }
  if (view.kind === "fileEdit") {
    const verb =
      view.created === true
        ? "created"
        : view.created === false && view.operation === "write_file"
          ? "overwrote"
          : tool.statusKind === "running"
            ? view.operation === "write_file"
              ? "writing"
              : "editing"
            : "edited";
    return (
      <>
        <span className="tool-kind">{verb}</span>
        <span className="tool-primary mono">{view.path ?? tool.toolName}</span>
        {view.replacementsApplied != null && view.replacementsApplied > 1 ? (
          <span className="tool-secondary">×{view.replacementsApplied}</span>
        ) : null}
        {commonBadges(tool)}
      </>
    );
  }
  if (view.kind === "agent") {
    return (
      <>
        <span className="tool-kind">agent · {view.action}</span>
        <span className="tool-primary">
          {view.name ?? view.sessionId ?? "agent"}
        </span>
        {commonBadges(tool)}
        {compact(view.description) ? (
          <span className="tool-secondary tool-summary-preview">
            {compact(view.description)}
          </span>
        ) : null}
      </>
    );
  }
  if (view.kind === "process") {
    return (
      <>
        <span className="tool-kind">process · {view.action}</span>
        <span className="tool-primary mono">
          {view.target ?? "background work"}
        </span>
        {commonBadges(tool)}
      </>
    );
  }
  if (view.kind === "mcp") {
    return (
      <>
        <span className="tool-kind">MCP</span>
        <span className="tool-primary">
          {view.selectedToolName ?? tool.toolName}
        </span>
        {view.serviceId ? (
          <span className="tool-secondary">{view.serviceId}</span>
        ) : null}
        {commonBadges(tool)}
      </>
    );
  }
  return (
    <>
      <span className="tool-primary">{tool.toolName}</span>
      {compact(view.summary) ? (
        <span className="tool-secondary tool-summary-preview">
          {compact(view.summary)}
        </span>
      ) : null}
      {commonBadges(tool)}
    </>
  );
}

function ExecutionDetails({ tool }: { tool: RenderedToolCallView }) {
  const command =
    tool.reconstruction.state === "ready" &&
    tool.presentation.kind === "command"
      ? tool.presentation
      : null;
  const fields = [
    ["Tool", tool.toolName],
    ["Status", statusLabel(tool)],
    [
      "Duration",
      command?.durationMs != null ? formatDuration(command.durationMs) : null,
    ],
    ["Working directory", command?.cwd],
    ["Exit code", command?.exitCode != null ? String(command.exitCode) : null],
    ["Execution", tool.awaitMode],
    ["Deadline", tool.deadlineAt],
    ["Sandbox", command?.executionMode],
    ["Network", command?.networkMode],
  ].filter((field): field is [string, string] => Boolean(field[1]));
  return (
    <details className="tool-execution-details">
      <summary>Execution details</summary>
      <dl>
        {fields.map(([label, value]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}

function ToolBody({
  tool,
  compactDetails = false,
  commandHint,
}: {
  tool: RenderedToolCallView;
  compactDetails?: boolean;
  commandHint?: string | null;
}) {
  const navigation = useContext(TranscriptNavigationContext);
  const view = tool.presentation;
  const payloadReady = tool.reconstruction.state === "ready";
  return (
    <div className="tool-item-body">
      {compactDetails &&
      commandHint &&
      (!payloadReady || view.kind !== "command") ? (
        <Payload label="Command" value={commandHint} />
      ) : null}
      {tool.cancelCause ? (
        <CancelCauseDetails cause={tool.cancelCause} />
      ) : null}
      {!compactDetails && (tool.awaitMode || tool.deadlineAt) ? (
        <div
          className="tool-meta muted small"
          data-testid={`tool-lifecycle-${tool.itemKey}`}
        >
          {tool.awaitMode ? <span>await: {tool.awaitMode}</span> : null}
          {tool.deadlineAt ? <span>deadline: {tool.deadlineAt}</span> : null}
        </div>
      ) : null}
      {!payloadReady ? <ToolPayloadAvailability tool={tool} /> : null}
      {payloadReady &&
        navigation &&
        (view.kind === "fileRead" || view.kind === "fileEdit") && (
          <div className="tool-meta">
            {(view.kind === "fileRead"
              ? view.operation === "read_file" && view.target
              : view.path) && (
              <button
                type="button"
                onClick={() =>
                  navigation.openFile(
                    (view.kind === "fileRead" ? view.target : view.path)!,
                  )
                }
              >
                Open file
              </button>
            )}
            {view.kind === "fileEdit" && view.path && view.diff.length > 0 && (
              <button
                type="button"
                onClick={() => navigation.openDiff(view.path!, view.diff)}
              >
                Review excerpt
              </button>
            )}
          </div>
        )}
      {payloadReady && view.kind === "command" ? (
        <>
          {compactDetails ? (
            <Payload label="Command" value={view.command} />
          ) : null}
          {!compactDetails &&
          (view.durationMs != null ||
            view.cwd ||
            view.executionMode ||
            view.networkMode) ? (
            <div className="tool-meta muted small">
              {view.durationMs != null ? (
                <span>{formatDuration(view.durationMs)}</span>
              ) : null}
              {view.cwd ? <span className="mono">{view.cwd}</span> : null}
              {view.executionMode ? (
                <span>sandbox: {view.executionMode}</span>
              ) : null}
              {view.networkMode ? (
                <span>network: {view.networkMode}</span>
              ) : null}
            </div>
          ) : null}
          <Payload
            label={compactDetails ? "Output" : "stdout"}
            value={view.stdout}
          />
          <Payload
            label={compactDetails ? "Errors" : "stderr"}
            value={view.stderr}
          />
          <Payload label="output" value={view.fallbackOutput} />
        </>
      ) : null}
      {payloadReady && view.kind === "fileRead" ? (
        <>
          <Payload label="contents" value={view.body} />
          <Payload label="output" value={view.fallbackOutput} />
        </>
      ) : null}
      {payloadReady && view.kind === "fileEdit" ? (
        <>
          {view.diff.length > 0 ? (
            <div className="tool-payload-wrap">
              <CopyButton
                className="tool-payload-copy"
                getText={() =>
                  view.diff
                    .map((line) => `${DIFF_MARK[line.kind]}${line.text}`)
                    .join("\n")
                }
              />
              <pre className="tool-diff">
                {view.diff.map((line, index) => (
                  <span
                    className={`tool-diff-line is-${line.kind}`}
                    key={`${line.kind}-${index}`}
                  >
                    <span aria-hidden="true">{DIFF_MARK[line.kind]}</span>
                    <span>{line.text}</span>
                  </span>
                ))}
              </pre>
            </div>
          ) : null}
          <Payload label="output" value={view.fallbackOutput} />
        </>
      ) : null}
      {payloadReady && view.kind === "agent" ? (
        <>
          <Payload
            label={view.action === "start" ? "assignment" : "message"}
            value={view.description}
          />
          {view.sessionId ? (
            <div className="tool-identity">
              <span className="tool-detail-label">session</span>
              <code>{view.sessionId}</code>
            </div>
          ) : null}
          <Payload label="result" value={view.output} />
        </>
      ) : null}
      {payloadReady && view.kind === "process" ? (
        <>
          <Payload label="arguments" value={view.description} />
          <Payload label="result" value={view.output} />
        </>
      ) : null}
      {payloadReady && view.kind === "mcp" ? (
        <>
          <Payload label="arguments" value={view.arguments} />
          <Payload label="result" value={view.output} />
        </>
      ) : null}
      {payloadReady && view.kind === "generic" ? (
        <>
          <Payload label="input" value={view.input} />
          <Payload label="result" value={view.output} />
        </>
      ) : null}
      <LiveOutput tool={tool} />
      {compactDetails ? <ExecutionDetails tool={tool} /> : null}
    </div>
  );
}

function UnifiedToolItem({
  tool,
  launcher,
  compactDetails = false,
  initiallyExpanded = false,
}: {
  tool: RenderedToolCallView;
  launcher?: RenderedToolCallView;
  compactDetails?: boolean;
  initiallyExpanded?: boolean;
}) {
  const [expanded, setExpanded] = useState(initiallyExpanded);
  const live =
    tool.statusKind.toLowerCase() === "running" &&
    Boolean(tool.partialOutputTail);
  return (
    <details
      className={`tool-item tool-item-${tool.presentation.kind}`}
      data-testid={`tool-${tool.itemKey}`}
      open={compactDetails ? expanded : live}
      onToggle={
        compactDetails
          ? (event) => setExpanded(event.currentTarget.open)
          : undefined
      }
    >
      <summary className="tool-item-summary">
        <span className={statusClass(actionStatus(tool))} aria-hidden="true" />
        <span className="tool-item-summary-content">
          {compactDetails ? (
            <ActionSummary tool={tool} launcher={launcher} />
          ) : (
            <ToolSummary tool={tool} />
          )}
        </span>
        <span aria-hidden="true" className="tool-item-action">
          <svg viewBox="0 0 16 16">
            <path d="m6 4 4 4-4 4" />
          </svg>
        </span>
      </summary>
      {!compactDetails || expanded ? (
        <ToolBody
          tool={tool}
          compactDetails={compactDetails}
          commandHint={actionCommand({ tool, launcher })}
        />
      ) : null}
      {launcher && expanded ? (
        <details className="tool-launch-details">
          <summary>Launch details</summary>
          <ToolBody tool={launcher} compactDetails />
        </details>
      ) : null}
    </details>
  );
}

export function ToolGroup({
  tools,
  compactDetails = false,
  expandedItemKey,
}: {
  tools: RenderedToolCallView[];
  compactDetails?: boolean;
  expandedItemKey?: string;
}) {
  return (
    <section className="tool-group">
      {(compactDetails
        ? actionEntries(tools)
        : tools.map((tool) => ({ tool, launcher: undefined }))
      ).map(({ tool, launcher }) => {
        const itemKey =
          expandedItemKey &&
          actionItemKeys({ tool, launcher }).includes(expandedItemKey)
            ? expandedItemKey
            : tool.itemKey;
        const denial =
          tool.statusKind.toLowerCase() === "error" ? tool.denial : null;
        return denial ? (
          <CommandDenialToolItem denial={denial} key={itemKey} tool={tool} />
        ) : (
          <UnifiedToolItem
            key={itemKey}
            tool={tool}
            launcher={launcher}
            compactDetails={compactDetails}
            initiallyExpanded={itemKey === expandedItemKey}
          />
        );
      })}
    </section>
  );
}

function ActionSummary(entry: ActionEntry) {
  const { tool } = entry;
  const title = actionTitle(entry);
  const kind = actionKind(entry);
  const command = actionCommand(entry);
  const status = actionStatus(tool);
  return (
    <>
      {kind ? (
        <span className="tool-kind">{kind === "command" ? "$" : kind}</span>
      ) : null}
      <span className={`tool-primary${command ? " mono" : ""}`} title={title}>
        {title}
      </span>
      {status !== "success" ? (
        <span className={`tool-action-status is-${status}`}>
          {statusLabel(tool)}
        </span>
      ) : null}
      {tool.cancelCause ? (
        <CancelCauseBadge
          cause={tool.cancelCause}
          className="tool-item-cause-badge"
        />
      ) : null}
    </>
  );
}

function formatDuration(ms: number) {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1).replace(/\.0$/, "")}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
}
