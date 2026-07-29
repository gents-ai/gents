import React from "react";
import ReactDOM from "react-dom/client";

import App from "../../src/App";
import { setDesktopShellTimingConfigForTests } from "../../src/hooks/useDesktopShell";
import { createDesktopUiHarness } from "./desktopHarness";
import { createLiveDesktopUiHarness } from "./liveBridgeHarness";

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

setDesktopShellTimingConfigForTests({
  clientRestartBackoffMs: 1,
  clientRestartMaxAttempts: 2,
  p2pAutoRestartCooldownMs: 10,
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App
      bridge={{
        api: harness.adapter,
        listenToUpdates: harness.listenerFactory,
      }}
    />
  </React.StrictMode>,
);
