import React from "react";
import ReactDOM from "react-dom/client";

import App from "../../src/App";
import { setDesktopApiAdapterForTests } from "../../src/lib/desktop-api";
import { setDesktopClientUpdatedListenerFactoryForTests } from "../../src/lib/desktop-events";
import { setDesktopShellTimingConfigForTests } from "../../src/hooks/useDesktopShell";
import { createDesktopUiHarness } from "./desktopHarness";

const scenario = new URLSearchParams(window.location.search).get("scenario");
const {
  adapter,
  listenerFactory,
  scenario: resolvedScenario,
} = createDesktopUiHarness({
  scenario,
});

document.documentElement.dataset.desktopUiHarnessScenario = resolvedScenario;

setDesktopApiAdapterForTests(adapter);
setDesktopClientUpdatedListenerFactoryForTests(listenerFactory);
setDesktopShellTimingConfigForTests({
  clientRestartBackoffMs: 1,
  clientRestartMaxAttempts: 2,
  p2pAutoRestartCooldownMs: 10,
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
