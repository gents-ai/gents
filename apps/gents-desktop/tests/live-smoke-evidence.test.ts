import { describe, expect, it } from "vitest";

import type {
  RequestDiagnostics,
  RequestDiagnosticsBundle,
} from "./live-bridge-runner";
import {
  formatLiveSmokeError,
  liveSmokeFailureSummary,
  liveSmokeSummary,
} from "./playwright-live/liveSmokeEvidence";

const runner = {
  baseUrl: "http://127.0.0.1:9292",
  deploymentLabel: "desktop-live",
  agentDid: "did:key:zLive",
  toolRoot: "/private/tmp/gents-live",
};

describe("live smoke evidence summaries", () => {
  it("summarizes successful live browser smoke diagnostics", () => {
    const summary = liveSmokeSummary({
      ...runner,
      sessionId: "session-1",
      requestId: "request-1",
      turnState: "completed",
      transcriptRows: 3,
      diagnostics: diagnosticsBundle(),
    });

    expect(summary).toContain("# Desktop Live Browser Smoke");
    expect(summary).toContain("Deployment: `desktop-live`");
    expect(summary).toContain("| Desktop timeline rows | `4` |");
    expect(summary).toContain("| Remote message rows | `7` |");
    expect(summary).toContain("desktop-live-browser-diagnostics.json");
    expect(summary).toContain("desktop-live-browser-final.png");
  });

  it("summarizes failure evidence when a request was not submitted", () => {
    const summary = liveSmokeFailureSummary({
      error: "bridge did not render",
      runner,
      submitted: null,
      diagnostics: null,
      screenshotAttached: false,
    });

    expect(summary).toContain("# Desktop Live Browser Smoke Failure");
    expect(summary).toContain("| Session | `not submitted` |");
    expect(summary).toContain("| Diagnostics attached | `no` |");
    expect(summary).toContain("failure screenshot was unavailable");
    expect(summary).toContain("no request diagnostics were available");
  });

  it("redacts provider secrets and escapes markdown table cells", () => {
    const error = new Error(
      "provider failed with sk-proj-secret_123 and Bearer abc.def|ghi `tick`",
    );
    const summary = liveSmokeFailureSummary({
      error,
      runner,
      submitted: {
        agentDid: "did:key:zLive",
        sessionId: "session-2",
        requestId: "request-2",
      },
      diagnostics: diagnosticsBundle(),
      screenshotAttached: true,
    });

    expect(formatLiveSmokeError(error)).toContain("sk-REDACTED");
    expect(summary).toContain("Bearer REDACTED");
    expect(summary).toContain("\\|");
    expect(summary).toContain("'tick'");
    expect(summary).not.toContain("sk-proj-secret_123");
    expect(summary).not.toContain("abc.def|ghi");
    expect(summary).toContain("desktop-live-browser-failure.png");
    expect(summary).toContain("desktop-live-browser-failure-diagnostics.json");
  });
});

function diagnosticsBundle(): RequestDiagnosticsBundle {
  return {
    desktop: requestDiagnostics("desktop", {
      timelineCount: 4,
      messageCount: 5,
    }),
    remote: requestDiagnostics("remote", {
      timelineCount: 6,
      messageCount: 7,
    }),
  };
}

function requestDiagnostics(
  source: string,
  counts: { timelineCount: number; messageCount: number },
): RequestDiagnostics {
  return {
    source,
    sessionId: "session-1",
    requestId: "request-1",
    turnState: "completed",
    latestRequestId: "request-1",
    conversationUpdatedAt: "2026-06-24T00:00:00Z",
    request: {
      status: "completed",
      lifecycleState: "terminal",
      failureReason: null,
      createdAt: "2026-06-24T00:00:00Z",
      claimedAt: "2026-06-24T00:00:01Z",
      interruptRequestedAt: null,
      validUntil: null,
    },
    response: {
      status: "completed",
      errorMessage: null,
      progressSeq: 3,
      materializedMessageSequence: 2,
      materializedAt: "2026-06-24T00:00:02Z",
      completedAt: "2026-06-24T00:00:03Z",
      contentLen: 42,
      reasoningLen: 0,
    },
    toolCalls: {
      total: 0,
      completed: 0,
      pending: 0,
      latestToolName: null,
      latestStatus: null,
      latestCompletedAt: null,
    },
    toolResultCount: 0,
    messageCount: counts.messageCount,
    timelineCount: counts.timelineCount,
    activeResponseOverlayContentLen: 0,
    activeResponseOverlayReasoningLen: 0,
  };
}
