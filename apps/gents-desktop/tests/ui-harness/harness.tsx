import React, { Profiler } from "react";
import ReactDOM from "react-dom/client";

import App from "../../src/App";
import { setTimingForTests } from "../../src/hooks/timing";
import { createDesktopUiHarness } from "./desktopHarness";
import { createLiveDesktopUiHarness } from "./liveBridgeHarness";
import { seedHarnessJoining } from "./sharingHarness";

declare global {
  interface Window {
    __GENTS_MOBILE_PERFORMANCE__?: NonNullable<
      ReturnType<typeof createDesktopUiHarness>["performance"]
    >;
    __GENTS_SESSION_SYNC__?: NonNullable<
      ReturnType<typeof createDesktopUiHarness>["sessionSync"]
    >;
  }
}

const params = new URLSearchParams(window.location.search);
const backend = params.get("backend") === "live" ? "live" : "deterministic";
const harness =
  backend === "live"
    ? createLiveDesktopUiHarness({ bridgeUrl: params.get("bridgeUrl") })
    : createDesktopUiHarness({
        scenario: params.get("scenario"),
        compactionUnmarked: params.has("compactionUnmarked"),
        compactionAt: params.has("compactionAt")
          ? Number(params.get("compactionAt"))
          : undefined,
        sessionIndexCount: params.has("sessions")
          ? Number(params.get("sessions"))
          : undefined,
      });

if (backend === "deterministic" && params.get("configApplyFailure") === "once") {
  const apply = harness.adapter.applyConfigComponents;
  let fail = true;
  harness.adapter.applyConfigComponents = async (request) => {
    if (fail) {
      fail = false;
      throw new Error("The agent could not save inference. Please retry.");
    }
    return apply(request);
  };
}

if (backend === "deterministic" && params.get("scenario") === "notices") {
  /* a usage limit with no account to move to, as the runtime reports it, and
     one with accounts to move to; the provider catalog can't be read */
  const resetsAt = new Date(Date.now() + (2 * 60 + 14) * 60_000).toISOString();
  harness.adapter.fetchProviderRecovery = async ({ sessionId }) =>
    sessionId === "session-notices-limit"
      ? {
          reason: "usage_limit",
          accountLabel: "work@example.com",
          profileId: "profile-claude",
          affectedAgents: ["default", "ops", "reviewer", "writer"],
          resetsAt,
          candidates: [],
          companionProfiles: [],
          switchCost: "",
          unavailableReason:
            "no other Claude account offers claude-sonnet-5-5; add one with `gents claude-login --label <label>`",
          goal: {
            goalId: "goal-limit",
            status: "usage_limited",
            autoResumeAtReset: false,
          },
        }
      : sessionId === "session-notices-move"
        ? {
            reason: "usage_limit",
            accountLabel: "work@example.com",
            profileId: "profile-claude",
            affectedAgents: ["default"],
            resetsAt,
            candidates: [
              { backendId: "backend-claude-personal", label: "personal@example.com" },
              { backendId: "backend-claude-team", label: "team@example.com" },
            ],
            companionProfiles: ["profile-claude-compact"],
            switchCost:
              "the next turn rebuilds the prompt cache on the new account (about 84k input tokens)",
            unavailableReason: null,
            goal: null,
          }
        : null;
  harness.adapter.getInferenceSetupCatalog = async () => {
    throw new Error("setup catalog: connection refused (os error 61)");
  };
}

if (backend === "deterministic") {
  seedHarnessJoining(params.get("scenario"));
}

document.documentElement.dataset.desktopUiHarnessBackend = backend;
document.documentElement.dataset.desktopUiHarnessScenario =
  "scenario" in harness ? harness.scenario : "live";
if ("bridgeUrl" in harness && harness.bridgeUrl) {
  document.documentElement.dataset.desktopUiHarnessBridgeUrl = harness.bridgeUrl;
}
if ("performance" in harness && harness.performance) {
  window.__GENTS_MOBILE_PERFORMANCE__ = harness.performance;
}
if ("sessionSync" in harness && harness.sessionSync) {
  window.__GENTS_SESSION_SYNC__ = harness.sessionSync;
}

setTimingForTests({
  clientRestartBackoffMs: 1,
  clientRestartMaxAttempts: 2,
  p2pAutoRestartCooldownMs: 10,
  ...(window.__GENTS_MOBILE_PERFORMANCE__ ? { sessionReadGapMs: null } : {}),
});

const app = (
  <App
    bridge={{
      api: harness.adapter,
      listenToUpdates: harness.listenerFactory,
      supportsManagedServer: params.get("scenario") === "startup-port-in-use",
      sharing:
        backend === "deterministic" && "sharing" in harness
          ? harness.sharing
          : undefined,
    }}
  />
);

/** Development Strict Mode replays mounts and inflates layout-read counts.
Performance fixtures measure mounting without those development-only replays. */
const tree = window.__GENTS_MOBILE_PERFORMANCE__ ? (
  <Profiler
    id="desktop-app"
    onRender={(id, phase, actualDuration, baseDuration, startTime, commitTime) => {
      window.__GENTS_MOBILE_PERFORMANCE__?.recordCommit({
        id,
        phase,
        actualDurationMs: actualDuration,
        baseDurationMs: baseDuration,
        startTimeMs: startTime,
        commitTimeMs: commitTime,
      });
    }}
  >
    {app}
  </Profiler>
) : (
  <React.StrictMode>{app}</React.StrictMode>
);

ReactDOM.createRoot(document.getElementById("root")!).render(tree);
