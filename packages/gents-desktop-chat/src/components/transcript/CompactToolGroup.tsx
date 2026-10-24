import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { ToolGroup } from "./ToolGroup.js";
import { isEmptyAssistantMessage } from "./MessageItems.js";
import {
  actionEntries,
  actionKind,
  actionItemKeys,
  actionStatus,
  runningAction,
  type ActionEntry,
} from "./actionPresentation.js";

function physicalGroupOwner(
  item: Extract<RenderedTimelineItem, { kind: "toolGroup" }>,
) {
  const owners = new Set(
    item.tools.map((tool) => tool.requestTurn?.requestDocId),
  );
  return owners.size === 1 ? owners.values().next().value : null;
}

type AssistantMessage = Extract<
  RenderedTimelineItem,
  { kind: "assistantMessage" }
>;

/** Invisible messages remain available to the host's turn-attachment renderer. */
export type CompactTimelineItem = RenderedTimelineItem & {
  compactAnchors?: readonly AssistantMessage[];
};

export function coalesceToolGroups(items: CompactTimelineItem[]) {
  const result: CompactTimelineItem[] = [];
  for (let index = 0; index < items.length; index += 1) {
    const item = items[index]!;
    if (item.kind !== "toolGroup") {
      result.push(item);
      continue;
    }
    const owner = physicalGroupOwner(item);
    if (!owner) {
      result.push(item);
      continue;
    }
    const chunks = [item.tools];
    const anchors = [...(item.compactAnchors ?? [])];
    let changed = false;
    while (index + 1 < items.length) {
      const next = items[index + 1]!;
      if (next.kind === "toolGroup" && physicalGroupOwner(next) === owner) {
        chunks.push(next.tools);
        anchors.push(...(next.compactAnchors ?? []));
      } else if (
        next.kind === "assistantMessage" &&
        next.requestTurn?.requestDocId === owner &&
        isEmptyAssistantMessage(next)
      ) {
        anchors.push(next);
      } else break;
      changed = true;
      index += 1;
    }
    result.push(
      changed
        ? { ...item, tools: chunks.flat(), compactAnchors: anchors }
        : item,
    );
  }
  return result;
}

function completedLabel(entries: ActionEntry[]) {
  const counts = new Map<string, number>();
  for (const entry of entries) {
    const { tool } = entry;
    const presentation =
      actionKind(entry) === "command"
        ? "command"
        : tool.reconstruction.state === "ready"
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
  expandedItemKey,
  onClose,
}: {
  tools: RenderedToolCallView[];
  expandedItemKey?: string;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const title = useId();
  useLayoutEffect(() => {
    const element = dialog.current!;
    element.showModal();
    element
      .querySelector(".tool-item[open]")
      ?.scrollIntoView({ block: "nearest" });
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
          Actions <span>{actionEntries(tools).length}</span>
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
        <ToolGroup
          tools={tools}
          compactDetails
          expandedItemKey={expandedItemKey}
        />
      </div>
    </dialog>,
    document.body,
  );
}

export function CompactToolGroup({ tools }: { tools: RenderedToolCallView[] }) {
  const [selected, setSelected] = useState<{
    itemKey: string;
    expand: boolean;
  } | null>(null);
  const buttons = useRef(new Map<string, HTMLButtonElement>());
  const focusAfterClose = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (selected || !focusAfterClose.current) return;
    buttons.current.get(focusAfterClose.current)?.focus();
    focusAfterClose.current = null;
  }, [selected]);
  if (tools.length === 0) return null;
  const settled: ActionEntry[] = [];
  const active: ActionEntry[] = [];
  for (const entry of actionEntries(tools)) {
    (actionStatus(entry.tool) === "running" ? active : settled).push(entry);
  }
  const sections: ActionEntry[][] = settled.length ? [settled] : [];
  if (active.length > 2) {
    sections.push(active.slice(0, -1), [active.at(-1)!]);
  } else {
    sections.push(...active.map((entry) => [entry]));
  }
  return (
    <>
      {sections.map((entries) => {
        const first = entries[0]!;
        const running = actionStatus(first.tool) === "running";
        const failures = entries.filter(
          (entry) => actionStatus(entry.tool) === "error",
        );
        const failed = failures.length > 0;
        const succeeded = entries.filter(
          (entry) => actionStatus(entry.tool) === "success",
        );
        const stopped = entries.filter(
          (entry) => actionStatus(entry.tool) === "cancelled",
        ).length;
        const unknown =
          entries.length - succeeded.length - failures.length - stopped;
        const failedLabel = `${failures.length} ${failures.length === 1 ? "action failed" : "actions failed"}`;
        const settledParts = [
          succeeded.length ? completedLabel(succeeded) : null,
          stopped
            ? `${stopped} ${stopped === 1 ? "action stopped" : "actions stopped"}`
            : null,
          unknown
            ? entries.length === 1
              ? "Action status unavailable"
              : `${unknown} ${unknown === 1 ? "status" : "statuses"} unavailable`
            : null,
        ].filter(Boolean);
        const caption = running
          ? entries.length === 1
            ? runningAction(first)
            : `${entries.length} actions running`
          : settledParts.join(" · ") || failedLabel;
        const label =
          !running && failed && settledParts.length
            ? `${caption} · ${failedLabel}`
            : caption;
        const selectedEntry =
          failures[0] ??
          entries.find((entry) => actionStatus(entry.tool) !== "success") ??
          first;
        return (
          <button
            key={first.tool.itemKey}
            ref={(element) => {
              for (const key of entries.flatMap(actionItemKeys)) {
                if (element) buttons.current.set(key, element);
                else buttons.current.delete(key);
              }
            }}
            type="button"
            className={`tool-activity-row${running ? " is-running" : ""}${failed ? " has-errors" : ""}`}
            aria-haspopup="dialog"
            aria-expanded={
              selected !== null &&
              entries.some((item) =>
                actionItemKeys(item).includes(selected.itemKey),
              )
            }
            aria-label={`View actions: ${label}`}
            onClick={() =>
              setSelected({
                itemKey: selectedEntry.tool.itemKey,
                expand: actionStatus(selectedEntry.tool) !== "success",
              })
            }
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
              {caption}
            </span>
            {!running && failed && settledParts.length > 0 ? (
              <span className="tool-activity-errors">
                {failures.length} failed
              </span>
            ) : null}
            <svg
              className="tool-activity-chevron"
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path d="m9 5 7 7-7 7" />
            </svg>
          </button>
        );
      })}
      {selected ? (
        <ActivityDetails
          tools={tools}
          expandedItemKey={selected.expand ? selected.itemKey : undefined}
          onClose={() => {
            focusAfterClose.current = selected.itemKey;
            setSelected(null);
          }}
        />
      ) : null}
    </>
  );
}
