import React, { Profiler } from "react";
import ReactDOM from "react-dom/client";

import App from "../../src/App";
import { setDesktopShellTimingConfigForTests } from "../../src/hooks/useDesktopShell";
import { createDesktopUiHarness } from "./desktopHarness";
import { createLiveDesktopUiHarness } from "./liveBridgeHarness";

declare global {
  interface Window {
    __GENTS_MOBILE_PERFORMANCE__?: NonNullable<
      ReturnType<typeof createDesktopUiHarness>["performance"]
    >;
  }
}

const params = new URLSearchParams(window.location.search);
const backend = params.get("backend") === "live" ? "live" : "deterministic";
const harness =
  backend === "live"
    ? createLiveDesktopUiHarness({ bridgeUrl: params.get("bridgeUrl") })
    : createDesktopUiHarness({ scenario: params.get("scenario") });

document.documentElement.dataset.desktopUiHarnessBackend = backend;
document.documentElement.dataset.desktopUiHarnessScenario =
  "scenario" in harness ? harness.scenario : "live";
if ("bridgeUrl" in harness && harness.bridgeUrl) {
  document.documentElement.dataset.desktopUiHarnessBridgeUrl = harness.bridgeUrl;
}
if ("performance" in harness && harness.performance) {
  window.__GENTS_MOBILE_PERFORMANCE__ = harness.performance;
}

setDesktopShellTimingConfigForTests({
  clientRestartBackoffMs: 1,
  clientRestartMaxAttempts: 2,
  p2pAutoRestartCooldownMs: 10,
});

const app = (
  <App
    bridge={{
      api: harness.adapter,
      listenToUpdates: harness.listenerFactory,
    }}
  />
);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {window.__GENTS_MOBILE_PERFORMANCE__ ? (
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
      app
    )}
  </React.StrictMode>,
);
