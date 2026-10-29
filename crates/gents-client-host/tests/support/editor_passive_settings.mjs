import { createRequire } from "node:module";
import { join, isAbsolute, resolve } from "node:path";
import { mkdir, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const extension = process.env.GENTS_VSCODE_ROOT;
if (!extension || !isAbsolute(extension))
  throw new Error(
    "GENTS_VSCODE_ROOT must name an absolute extension checkout.",
  );
const requireFromExtension = createRequire(join(extension, "package.json"));
const { _electron: electron, expect } =
  requireFromExtension("@playwright/test");
const { resolveCliArgsFromVSCodeExecutablePath } = requireFromExtension(
  "@vscode/test-electron",
);
const { closeEditor } = await import(
  pathToFileURL(join(extension, "tests/editorLifecycle.mjs"))
);
const { installExtension } = await import(
  pathToFileURL(join(extension, "tests/installExtension.mjs"))
);

const [root, home, nodeDid] = process.argv.slice(2);
const executablePath = process.env.GENTS_VSCODE_EXECUTABLE;
const vsix = process.env.GENTS_VSCODE_VSIX;
if (!root || !isAbsolute(root) || !home || !nodeDid || !executablePath || !vsix)
  throw new Error(
    "Use the native passive-settings fixture and an installed VSIX.",
  );
const profile = join(root, "settings-profile");
const extensions = join(root, "settings-extensions");
const workspace = join(root, "settings-workspace");
const artifacts = join(root, "editor-passive-settings-artifacts");
await mkdir(workspace, { recursive: true });
await mkdir(artifacts, { recursive: true });
await mkdir(join(profile, "User"), { recursive: true });
await writeFile(
  join(profile, "User", "settings.json"),
  JSON.stringify({
    "gents.clientHome": home,
    "editor.accessibilitySupport": "off",
    "workbench.startupEditor": "none",
    "workbench.enableExperiments": false,
    "update.mode": "none",
    "telemetry.telemetryLevel": "off",
    "window.restoreWindows": "none",
  }),
);
const [cli, ...args] = resolveCliArgsFromVSCodeExecutablePath(executablePath, {
  reuseMachineInstall: true,
});
await installExtension(
  cli,
  [
    ...args,
    `--user-data-dir=${profile}`,
    `--extensions-dir=${extensions}`,
    "--install-extension",
    resolve(vsix),
    "--force",
  ],
  { timeout: 60_000 },
);
const env = { ...process.env };
delete env.ELECTRON_RUN_AS_NODE;
const app = await electron.launch({
  executablePath,
  env,
  args: [
    `--user-data-dir=${profile}`,
    `--extensions-dir=${extensions}`,
    "--disable-workspace-trust",
    "--disable-updates",
    "--disable-telemetry",
    "--skip-welcome",
    "--skip-release-notes",
    "--new-window",
    workspace,
  ],
  timeout: 30_000,
});
const processHandle = app.process();
try {
  const page = await app.firstWindow();
  page.setDefaultTimeout(30_000);
  await page.locator(".monaco-workbench").waitFor();
  const frame = page
    .frameLocator("iframe.webview.ready")
    .frameLocator("iframe");
  const trigger = frame.getByRole("button", {
    name: /^Choose model: editor-fixture/,
  });
  await expect(trigger).toBeEnabled();
  await frame.locator("body").evaluate((body) => {
    const view = body.ownerDocument.defaultView;
    view.__passiveSettings = [];
    view.__passiveReconnects = 0;
    view.addEventListener("message", ({ data }) => {
      if (data?.kind === "reconnected") view.__passiveReconnects += 1;
      const settings = data?.result;
      if (
        data?.kind !== "response" ||
        !Array.isArray(settings?.fastModeBackendIds)
      )
        return;
      view.__passiveSettings.push({
        nodeDid: settings.nodeDid,
        nestedNodeDid: settings.node?.nodeDid,
        models: settings.inferenceProfiles.map((profile) => profile.model_name),
        backends: settings.inferenceBackends.length,
        redacted: settings.inferenceBackends.every(
          (backend) =>
            backend.apiKey === undefined &&
            backend.auth === undefined &&
            backend.apiKeyEnvVar == null,
        ),
      });
    });
  });
  const input = frame.getByRole("textbox", { name: "Message", exact: true });
  const draft =
    "Retain this draft while refreshing model settings and reconnecting.";
  await input.fill(draft);
  async function refresh() {
    const before = await frame
      .locator("body")
      .evaluate(
        (body) => body.ownerDocument.defaultView.__passiveSettings.length,
      );
    await trigger.click();
    const picker = frame.getByRole("dialog", { name: "Model and reasoning" });
    await expect(
      picker.getByRole("option", { name: /^editor-fixture / }),
    ).toBeVisible();
    await expect
      .poll(() =>
        frame
          .locator("body")
          .evaluate(
            (body) => body.ownerDocument.defaultView.__passiveSettings.length,
          ),
      )
      .toBeGreaterThan(before);
    await picker.press("Escape");
    await expect(input).toHaveValue(draft);
  }
  await refresh();
  await page.keyboard.press("Meta+Shift+P");
  const commands = page.locator(".quick-input-widget input");
  await commands.fill(">Gents: Reconnect Client");
  await page.getByRole("option", { name: /Gents: Reconnect Client/ }).click();
  await expect
    .poll(() =>
      frame
        .locator("body")
        .evaluate((body) => body.ownerDocument.defaultView.__passiveReconnects),
    )
    .toBe(1);
  await expect(trigger).toBeEnabled();
  await refresh();
  const observations = await frame
    .locator("body")
    .evaluate((body) => body.ownerDocument.defaultView.__passiveSettings);
  expect(observations.length).toBeGreaterThanOrEqual(2);
  for (const observation of observations) {
    expect(observation.nodeDid).toBe(nodeDid);
    expect(observation.nestedNodeDid).toBe(nodeDid);
    expect(observation.models).toContain("editor-fixture");
    expect(observation.backends).toBeGreaterThan(0);
    expect(observation.redacted).toBe(true);
  }
  await writeFile(
    join(root, "installed-passive-settings.json"),
    JSON.stringify({ observations, draftPreserved: true }),
  );
  await writeFile(
    join(artifacts, "observations.json"),
    JSON.stringify({ observations, draftPreserved: true }),
  );
  await page.screenshot({ path: join(artifacts, "model-settings.png") });
} catch (error) {
  const page = await app.firstWindow();
  await page
    .screenshot({ path: join(artifacts, "failed.png") })
    .catch(() => {});
  await writeFile(
    join(root, "installed-passive-settings-failed.html"),
    await page.content(),
  );
  throw error;
} finally {
  await closeEditor(app, processHandle);
}
