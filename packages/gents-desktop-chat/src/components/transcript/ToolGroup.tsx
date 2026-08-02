import { parseCommandDenial } from "@source-inc/gents-desktop-client";
import type {
  RenderedToolCallView,
  ToolDetailValueView,
} from "@source-inc/gents-desktop-client";

import { CancelCauseBadge, CancelCauseDetails } from "../cancelUx/index.js";
import { CodeToolItem } from "../codeTools/CodeToolItem.js";
import { toCodeToolView } from "../codeTools/codeTools.js";
import { CommandDenialToolItem } from "../commandDenial/index.js";

function toolStatusClass(statusKind?: string | null) {
  switch ((statusKind ?? "").toLowerCase()) {
    case "success":
      return "tool-item-dot tool-item-dot-success";
    case "error":
      return "tool-item-dot tool-item-dot-error";
    case "awaitingapproval":
      return "tool-item-dot tool-item-dot-held";
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

const SAFE_TOOL_ARG_PREVIEW_FIELDS = new Set([
  "path",
  "file_path",
  "directory",
  "cwd",
  "pattern",
  "query",
  "command",
]);

const SENSITIVE_TOOL_ARG_PREVIEW =
  /(?:^|[^a-z0-9])(?:api[-_]?key|access[-_]?token|refresh[-_]?token|token|password|passwd|secret|authorization|cookie)(?:[^a-z0-9]|$)|\bbearer\s+\S+|\b(?:sk|gh[pousr]|xox[baprs])[-_][a-z0-9_-]{8,}/i;

function toolArgsPreview(args?: ToolDetailValueView | null): string | null {
  if (!args) {
    return null;
  }
  const source = args.fields.find((field) => {
    const key = field.key.trim().toLowerCase().replace(/-/g, "_");
    return SAFE_TOOL_ARG_PREVIEW_FIELDS.has(key) && field.value.trim();
  })?.value;
  const flat = source?.replace(/\s+/g, " ").trim();
  if (!flat || SENSITIVE_TOOL_ARG_PREVIEW.test(flat)) {
    return null;
  }
  return flat.length > 64 ? `${flat.slice(0, 64)}…` : flat;
}

function normalizedFieldKey(key: string) {
  return key
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "");
}

function toolDetailField(
  value: ToolDetailValueView | null | undefined,
  keys: string[],
): string | null {
  const normalizedKeys = new Set(keys.map(normalizedFieldKey));
  const field = value?.fields.find((candidate) =>
    normalizedKeys.has(normalizedFieldKey(candidate.key)),
  );
  return field?.value.trim() || null;
}

function safeSubagentPreview(value: string | null, maxLength: number) {
  const flat = value?.replace(/\s+/g, " ").trim();
  if (!flat || SENSITIVE_TOOL_ARG_PREVIEW.test(flat)) {
    return null;
  }
  return flat.length > maxLength ? `${flat.slice(0, maxLength)}…` : flat;
}

function isSubagentSpawnTool(toolName: string) {
  const normalized = toolName.trim().toLowerCase();
  return (
    normalized === "spawn_subagent" || normalized.endsWith(".spawn_subagent")
  );
}

function subagentStatusLabel(
  statusKind?: string | null,
  status?: string | null,
) {
  switch ((statusKind ?? "").toLowerCase()) {
    case "success":
      return "completed";
    case "error":
      return status?.trim() || "failed";
    case "awaitingapproval":
      return "awaiting approval";
    default:
      return "working";
  }
}

function SubagentToolItem({ tool }: { tool: RenderedToolCallView }) {
  const statusKind = (tool.statusKind ?? "running").toLowerCase();
  const name =
    toolDetailField(tool.args, ["name", "behavior_id", "behaviorId"]) ??
    "subagent";
  const prompt = toolDetailField(tool.args, ["prompt", "task"]);
  const promptPreview = safeSubagentPreview(prompt, 96);
  const awaitMode =
    tool.awaitMode ??
    toolDetailField(tool.args, ["await_mode", "awaitMode"]) ??
    null;
  const childRequestId =
    tool.childRequestId ??
    toolDetailField(tool.result, ["child_request_id", "childRequestId"]) ??
    null;
  const liveTail =
    statusKind === "running" ? (tool.partialOutputTail ?? null) : null;
  const statusLabel = subagentStatusLabel(tool.statusKind, tool.status);

  return (
    <details
      className="tool-item subagent-tool-item"
      data-child-request-id={childRequestId ?? undefined}
      data-testid={`subagent-tool-${tool.itemKey}`}
      open={liveTail != null || statusKind === "running"}
    >
      <summary className="tool-item-summary subagent-tool-summary">
        <span className="tool-item-summary-left subagent-tool-summary-left">
          <span
            aria-hidden="true"
            className={toolStatusClass(tool.statusKind)}
          />
          <span className="subagent-tool-kind">subagent</span>
          <span className="tool-item-name">{name}</span>
          {awaitMode ? (
            <span className="subagent-tool-mode">{awaitMode}</span>
          ) : null}
          <span
            className={`subagent-tool-status subagent-tool-status-${statusKind}`}
          >
            {statusLabel}
          </span>
          {promptPreview ? (
            <span className="tool-item-preview subagent-tool-preview">
              {promptPreview}
            </span>
          ) : null}
        </span>
        <span aria-hidden="true" className="tool-item-action">
          ▸
        </span>
      </summary>
      <div className="tool-item-body subagent-tool-body">
        {tool.cancelCause ? (
          <CancelCauseDetails cause={tool.cancelCause} />
        ) : null}
        {prompt ? (
          <div className="subagent-tool-assignment">
            <div className="tool-detail-label">assignment</div>
            <div>{prompt}</div>
          </div>
        ) : (
          <ToolDetailSection label="args" value={tool.args} />
        )}
        {childRequestId ? (
          <div className="subagent-tool-child">
            <span className="tool-detail-label">child request</span>
            <code>{childRequestId}</code>
          </div>
        ) : null}
        {liveTail != null ? (
          <div
            className="tool-live-tail"
            data-testid={`subagent-live-${tool.itemKey}`}
          >
            <span className="tool-live-tail-label">
              child activity
              <span aria-hidden="true" className="tool-live-dot" />
            </span>
            <pre>{liveTail}</pre>
          </div>
        ) : null}
        <ToolDetailSection label="result" value={tool.result} />
      </div>
    </details>
  );
}

export function ToolGroup({ tools }: { tools: RenderedToolCallView[] }) {
  return (
    <section className="tool-group">
      {tools.map((tool) => {
        const denial =
          (tool.statusKind ?? "").toLowerCase() === "error"
            ? (tool.denial ?? parseCommandDenial(tool.result?.rawText))
            : null;
        if (denial) {
          return (
            <CommandDenialToolItem
              denial={denial}
              key={tool.itemKey}
              tool={tool}
            />
          );
        }

        if (isSubagentSpawnTool(tool.toolName)) {
          return <SubagentToolItem key={tool.itemKey} tool={tool} />;
        }

        const codeView =
          (tool.statusKind ?? "").toLowerCase() === "success" &&
          !tool.cancelCause
            ? toCodeToolView(tool)
            : null;
        if (codeView) {
          return <CodeToolItem key={tool.itemKey} view={codeView} />;
        }
        const argsPreview = toolArgsPreview(tool.args);
        const liveTail =
          (tool.statusKind ?? "").toLowerCase() === "running"
            ? (tool.partialOutputTail ?? null)
            : null;
        return (
          <details
            className="tool-item"
            key={tool.itemKey}
            open={liveTail != null}
          >
            <summary className="tool-item-summary">
              <span className="tool-item-summary-left">
                <span
                  aria-hidden="true"
                  className={toolStatusClass(tool.statusKind)}
                />
                <span className="tool-item-name">{tool.toolName}</span>
                {argsPreview ? (
                  <span className="tool-item-preview">{argsPreview}</span>
                ) : null}
                {tool.cancelCause ? (
                  <CancelCauseBadge
                    cause={tool.cancelCause}
                    className="tool-item-cause-badge"
                  />
                ) : null}
                {(tool.statusKind ?? "").toLowerCase() ===
                "awaitingapproval" ? (
                  <span
                    className="tool-item-held-badge"
                    data-testid={`tool-held-${tool.itemKey}`}
                  >
                    awaiting approval
                  </span>
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
              {liveTail != null ? (
                <div
                  className="tool-live-tail"
                  data-testid={`tool-live-${tool.itemKey}`}
                >
                  <span className="tool-live-tail-label">
                    live output
                    <span aria-hidden="true" className="tool-live-dot" />
                  </span>
                  <pre>{liveTail}</pre>
                </div>
              ) : null}
              <ToolDetailSection label="result" value={tool.result} />
            </div>
          </details>
        );
      })}
    </section>
  );
}
