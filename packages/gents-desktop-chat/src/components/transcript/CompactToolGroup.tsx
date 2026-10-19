import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { ToolGroup } from "./ToolGroup.js";

export function coalesceToolGroups(items: RenderedTimelineItem[]) {
  const result: RenderedTimelineItem[] = [];
  for (const item of items) {
    const previous = result[result.length - 1];
    if (previous?.kind === "toolGroup" && item.kind === "toolGroup") {
      result[result.length - 1] = {
        ...previous,
        tools: [...previous.tools, ...item.tools],
      };
    } else {
      result.push(item);
    }
  }
  return result;
}

function runningLabel(tool: RenderedToolCallView) {
  if (tool.reconstruction.state !== "ready") return `Running ${tool.toolName}`;
  const view = tool.presentation;
  switch (view.kind) {
    case "command":
      return `Running ${view.command}`;
    case "fileRead": {
      const verb =
        view.operation === "grep"
          ? "Searching"
          : view.operation === "list_files" || view.operation === "glob"
            ? "Exploring"
            : "Reading";
      return `${verb} ${view.target ?? "files"}`;
    }
    case "fileEdit":
      return `Editing ${view.path ?? "a file"}`;
    case "agent":
      return view.description ?? `Working with ${view.name ?? "an agent"}`;
    case "process":
      return view.description ?? `Running ${view.target ?? "a process"}`;
    case "mcp":
      return `Running ${view.selectedToolName ?? tool.toolName}`;
    case "generic":
      return view.summary ?? `Running ${tool.toolName}`;
  }
}

function completedLabel(tools: RenderedToolCallView[]) {
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const presentation =
      tool.reconstruction.state === "ready"
        ? tool.presentation.kind
        : "generic";
    const kind = ["fileRead", "fileEdit", "command"].includes(presentation)
      ? presentation
      : "generic";
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return [...counts]
    .map(([kind, count]) => {
      switch (kind) {
        case "fileRead":
          return "explored files";
        case "fileEdit":
          return "edited files";
        case "command":
          return `ran ${count} ${count === 1 ? "command" : "commands"}`;
        default:
          return `ran ${count} ${count === 1 ? "action" : "actions"}`;
      }
    })
    .join(", ")
    .replace(/^./, (letter) => letter.toUpperCase());
}

function ActivityDetails({
  tools,
  onClose,
}: {
  tools: RenderedToolCallView[];
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const title = useId();
  useLayoutEffect(() => {
    const element = dialog.current!;
    element.showModal();
    return () => element.close();
  }, []);
  return createPortal(
    <dialog
      ref={dialog}
      className="tool-activity-dialog"
      aria-labelledby={title}
      onClose={onClose}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const bounds = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX < bounds.left ||
          event.clientX > bounds.right ||
          event.clientY < bounds.top ||
          event.clientY > bounds.bottom
        ) {
          event.currentTarget.close();
        }
      }}
    >
      <header className="tool-activity-header">
        <h2 id={title}>
          Actions <span>{tools.length}</span>
        </h2>
        <button
          type="button"
          aria-label="Close actions"
          onClick={() => dialog.current?.close()}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m6 6 12 12M6 18 18 6" />
          </svg>
        </button>
      </header>
      <div className="tool-activity-body">
        <ToolGroup tools={tools} compactDetails />
      </div>
    </dialog>,
    document.body,
  );
}

export function CompactToolGroup({ tools }: { tools: RenderedToolCallView[] }) {
  const [open, setOpen] = useState(false);
  if (tools.length === 0) return null;
  const running = [...tools]
    .reverse()
    .find((tool) => tool.statusKind.toLowerCase() === "running");
  const failed = tools.filter(
    (tool) => tool.statusKind.toLowerCase() === "error",
  ).length;
  const unknown = tools.some(
    (tool) =>
      !["running", "success", "error"].includes(tool.statusKind.toLowerCase()),
  );
  const label = running
    ? runningLabel(running)
    : unknown
      ? "Action status unavailable"
      : failed
        ? "Actions finished with errors"
        : completedLabel(tools);
  return (
    <>
      <button
        type="button"
        className={`tool-activity-row${running ? " is-running" : ""}${failed ? " has-errors" : ""}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`View actions: ${label}`}
        onClick={() => setOpen(true)}
      >
        <svg
          className="tool-activity-icon"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          {running ? (
            <circle cx="12" cy="12" r="8" />
          ) : (
            <path d="M4 4h6a3 3 0 0 1 3 3v14a4 4 0 0 0-4-2H4V4Zm16 0h-4a3 3 0 0 0-3 3m0 14a4 4 0 0 1 4-2h3V4Z" />
          )}
        </svg>
        <span className="tool-activity-label" title={label}>
          {label}
        </span>
        {running && tools.length > 1 ? (
          <span className="tool-activity-count">{tools.length}</span>
        ) : null}
        {failed ? (
          <span className="tool-activity-errors">{failed} failed</span>
        ) : null}
        <svg
          className="tool-activity-chevron"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          <path d="m9 5 7 7-7 7" />
        </svg>
      </button>
      {open ? (
        <ActivityDetails tools={tools} onClose={() => setOpen(false)} />
      ) : null}
    </>
  );
}
