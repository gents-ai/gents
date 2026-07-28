export type DiffLineKind = "add" | "del";
export type DiffLine = { kind: DiffLineKind; text: string };

export type FileEditView = {
  kind: "fileEdit";
  path: string;
  created: boolean;
  /** True when write_file replaced content that cannot be reconstructed. */
  overwrite: boolean;
  replacementsApplied: number;
  diff: DiffLine[];
};

export type CommandRunView = {
  kind: "command";
  command: string;
  exitCode: number | null;
  executionMode: string | null;
  networkMode: string | null;
  durationMs: number | null;
  cwd: string | null;
  timedOut: boolean;
  failed: boolean;
  stdout: string;
  stderr: string;
};

export type FileReadTool = "read_file" | "grep" | "glob" | "list_files";

export type FileReadView = {
  kind: "fileRead";
  tool: FileReadTool;
  target: string | null;
  returnedCount: number | null;
  totalCount: number | null;
  truncated: boolean;
  body: string;
};

export type CodeToolView = FileEditView | CommandRunView | FileReadView;

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) {
    return `${(ms / 1000).toFixed(1).replace(/\.0$/, "")}s`;
  }
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
}

const ANSI_PATTERN =
  // eslint-disable-next-line no-control-regex
  /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g;

export function stripAnsi(value: string): string {
  return value.replace(ANSI_PATTERN, "");
}
