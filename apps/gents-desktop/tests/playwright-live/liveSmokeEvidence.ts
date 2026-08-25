import type { RequestDiagnosticsBundle } from "../live-bridge-runner";

export type SubmittedRequest = {
  agentDid: string;
  requestId: string;
  sessionId: string;
};

export type LiveSmokeRunnerInfo = {
  baseUrl: string;
  deploymentLabel: string;
  agentDid: string;
  toolRoot: string;
};

export type LiveSmokeEvidence = LiveSmokeRunnerInfo & {
  sessionId: string;
  requestId: string;
  turnState: string;
  transcriptRows: number;
  transcriptQueryCount: number;
  transcriptQueriedRows: number;
  transcriptMessageQueryLimit: number;
  transcriptToolCallQueryLimit: number;
  diagnostics: RequestDiagnosticsBundle;
};

export type LiveSmokeFailureEvidence = {
  error: unknown;
  runner: LiveSmokeRunnerInfo;
  submitted: SubmittedRequest | null;
  diagnostics: RequestDiagnosticsBundle | null;
  screenshotAttached: boolean;
};

export function liveSmokeSummary(evidence: LiveSmokeEvidence) {
  return [
    "# Desktop Live Browser Smoke",
    "",
    `Deployment: \`${evidence.deploymentLabel}\``,
    `Bridge URL: \`${evidence.baseUrl}\``,
    `Agent DID: \`${evidence.agentDid}\``,
    `Tool root: \`${evidence.toolRoot}\``,
    "",
    "| Field | Value |",
    "| --- | --- |",
    `| Session | \`${evidence.sessionId}\` |`,
    `| Request | \`${evidence.requestId}\` |`,
    `| Turn state | \`${evidence.turnState}\` |`,
    `| Transcript rows | \`${evidence.transcriptRows}\` |`,
    `| Transcript DefraDB queries | \`${evidence.transcriptQueryCount}\` |`,
    `| Transcript rows queried | \`${evidence.transcriptQueriedRows}\` |`,
    `| Message query limit | \`${evidence.transcriptMessageQueryLimit}\` |`,
    `| Tool-call query limit | \`${evidence.transcriptToolCallQueryLimit}\` |`,
    `| Desktop timeline rows | \`${evidence.diagnostics.desktop.timelineCount}\` |`,
    `| Remote timeline rows | \`${evidence.diagnostics.remote.timelineCount}\` |`,
    `| Desktop message rows | \`${evidence.diagnostics.desktop.messageCount}\` |`,
    `| Remote message rows | \`${evidence.diagnostics.remote.messageCount}\` |`,
    "",
    "Artifacts:",
    "",
    "- `desktop-live-browser-diagnostics.json`",
    "- `desktop-live-browser-final.png`",
    "",
  ].join("\n");
}

export function liveSmokeFailureSummary(evidence: LiveSmokeFailureEvidence) {
  return [
    "# Desktop Live Browser Smoke Failure",
    "",
    `Deployment: \`${evidence.runner.deploymentLabel}\``,
    `Bridge URL: \`${evidence.runner.baseUrl}\``,
    `Agent DID: \`${evidence.runner.agentDid}\``,
    `Tool root: \`${evidence.runner.toolRoot}\``,
    "",
    "| Field | Value |",
    "| --- | --- |",
    `| Session | \`${evidence.submitted?.sessionId ?? "not submitted"}\` |`,
    `| Request | \`${evidence.submitted?.requestId ?? "not submitted"}\` |`,
    `| Error | \`${formatLiveSmokeError(evidence.error)}\` |`,
    `| Diagnostics attached | \`${evidence.diagnostics ? "yes" : "no"}\` |`,
    "",
    "Artifacts:",
    "",
    evidence.screenshotAttached
      ? "- `desktop-live-browser-failure.png`"
      : "- failure screenshot was unavailable",
    evidence.diagnostics
      ? "- `desktop-live-browser-failure-diagnostics.json`"
      : "- no request diagnostics were available",
    "",
  ].join("\n");
}

export function formatLiveSmokeError(error: unknown) {
  const message =
    error instanceof Error ? `${error.message}\n${error.stack ?? ""}` : String(error);
  return sanitizeMarkdownCell(message).slice(0, 2_000);
}

function sanitizeMarkdownCell(value: string) {
  return value
    .replace(/sk-[A-Za-z0-9_-]+/g, "sk-REDACTED")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer REDACTED")
    .replace(/\s+/g, " ")
    .replace(/\|/g, "\\|")
    .replace(/`/g, "'")
    .trim();
}
