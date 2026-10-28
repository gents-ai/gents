import {
  actionKind,
  actionStatus,
  type ActionEntry,
} from "./actionPresentation.js";

type ActivityKind =
  | "edit"
  | "read"
  | "search"
  | "explore"
  | "command"
  | "agent"
  | "fileAction"
  | "action";

const order: ActivityKind[] = [
  "edit",
  "read",
  "search",
  "explore",
  "command",
  "agent",
  "fileAction",
  "action",
];

function activityKind(entry: ActionEntry): ActivityKind {
  if (actionKind(entry) === "command") return "command";
  const { tool } = entry;
  if (tool.reconstruction.state !== "ready") return "action";
  switch (tool.presentation.kind) {
    case "fileEdit":
      return tool.presentation.applied === true ? "edit" : "fileAction";
    case "fileRead":
      if (actionStatus(tool) !== "success") return "fileAction";
      switch (tool.presentation.operation) {
        case "read_file":
          return "read";
        case "grep":
        case "glob":
          return "search";
        default:
          return "explore";
      }
    case "agent":
      return "agent";
    default:
      return "action";
  }
}

export function completedActivityLabel(entries: ActionEntry[]): string {
  const counts = new Map<ActivityKind, number>();
  for (const entry of entries) {
    const kind = activityKind(entry);
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return order
    .flatMap((kind) => {
      const count = counts.get(kind);
      if (!count) return [];
      switch (kind) {
        case "edit":
          return ["edited files"];
        case "read":
          return ["read files"];
        case "search":
          return ["searched files"];
        case "explore":
          return ["explored files"];
        case "command":
          return [`ran ${count} ${count === 1 ? "command" : "commands"}`];
        case "agent":
          return [`ran ${count} agent ${count === 1 ? "action" : "actions"}`];
        case "fileAction":
          return [`ran ${count} file ${count === 1 ? "action" : "actions"}`];
        case "action":
          return [`ran ${count} ${count === 1 ? "action" : "actions"}`];
      }
    })
    .join(", ")
    .replace(/^./, (letter) => letter.toUpperCase());
}

export function ActivityIcon({
  entries,
  running,
}: {
  entries: ActionEntry[];
  running: boolean;
}) {
  const kinds = new Set(entries.map(activityKind));
  const kind = order.find((candidate) => kinds.has(candidate)) ?? "action";
  return (
    <svg
      className="tool-activity-icon"
      data-activity-kind={running ? "running" : kind}
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      {running ? (
        <circle cx="12" cy="12" r="8" />
      ) : kind === "edit" ? (
        <path d="m16 3 5 5M3 21l1-6L16 3a3.5 3.5 0 0 1 5 5L9 20l-6 1Z" />
      ) : kind === "search" ? (
        <>
          <circle cx="10.5" cy="10.5" r="6.5" />
          <path d="m16 16 5 5" />
        </>
      ) : kind === "read" ? (
        <path d="M4 4h6a3 3 0 0 1 3 3v14a4 4 0 0 0-4-2H4V4Zm16 0h-4a3 3 0 0 0-3 3m0 14a4 4 0 0 1 4-2h3V4Z" />
      ) : kind === "explore" ? (
        <path d="M3 7V5h6l2 2h10v13H3V7Z" />
      ) : kind === "command" ? (
        <>
          <rect x="3" y="4" width="18" height="16" rx="3" />
          <path d="m7 8 3 3-3 3m6 1h4" />
        </>
      ) : kind === "agent" ? (
        <>
          <circle cx="12" cy="8" r="3" />
          <path d="M6 21v-3a6 6 0 0 1 12 0v3" />
        </>
      ) : kind === "fileAction" ? (
        <path d="M14 3H5v18h14V8l-5-5Zm0 0v5h5M8 12h8m-8 4h5" />
      ) : (
        <>
          <path d="m12 3 9 9-9 9-9-9 9-9Z" />
          <path d="M8 12h8m-4-4v8" />
        </>
      )}
    </svg>
  );
}
