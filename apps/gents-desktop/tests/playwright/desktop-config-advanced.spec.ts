import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openConfig,
  openConfigTab,
  saveConfig,
  test,
} from "./desktopTest";

test.describe("desktop config workspace advanced flows", () => {
  test("agent identity editing updates the workspace and chat shell", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigTab(page, "agent");

    await page.getByTestId("agent-edit-display-name").click();
    await page.getByTestId("agent-display-name").fill("Fleet Steward Harness");
    await saveConfig(page, "agent-save");
    await expect(page.locator(".config-title-block h1")).toHaveText(
      "Fleet Steward Harness",
    );

    await page.getByTestId("config-back-tab").click();
    await expect(page.locator(".connected-peer-card")).toContainText(
      "Fleet Steward Harness",
    );
    await expectNoPageHorizontalOverflow(page);
  });

  test("behavior create shortcuts jump to the matching document editors", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigTab(page, "behavior");

    await page.getByTestId("behavior-create-backend").click();
    await expect(page.getByTestId("config-tab-backends")).toHaveClass(/selected/);
    await expect(page.getByTestId("backend-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("backend-id").fill("backend-created-from-behavior");
    await page.getByTestId("backend-name").fill("Created backend");
    await page.getByTestId("backend-endpoint").fill("http://localhost:11434/v1");
    await page.getByTestId("backend-models").fill("llama3.2\nqwen2.5");
    await saveConfig(page, "backend-save");

    await openConfigTab(page, "behavior");
    await page.getByTestId("behavior-create-profile").click();
    await expect(page.getByTestId("config-tab-profiles")).toHaveClass(/selected/);
    await expect(page.getByTestId("profile-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("profile-id").fill("profile-created-from-behavior");
    await page.getByTestId("profile-display-name").fill("Created profile");
    await saveConfig(page, "profile-save");

    await openConfigTab(page, "behavior");
    await page.getByTestId("behavior-create-tool-selection").click();
    await expect(page.getByTestId("config-tab-toolSelections")).toHaveClass(/selected/);
    await expect(page.getByTestId("tool-selection-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("tool-selection-id").fill("tools-created-from-behavior");
    await page.getByTestId("tool-selection-display-name").fill("Created tools");
    await saveConfig(page, "tool-selection-save");
  });

  test("tool, schedule, and event-trigger editors preserve advanced controls", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);

    await openConfigTab(page, "toolSelections");
    await page
      .getByTestId("tool-command-execution-policy")
      .selectOption("workspace_write");
    await page.getByTestId("tool-command-network-mode").selectOption("enabled");
    await page.getByTestId("tool-command-forbidden-argv-prefixes").fill("rm -rf\ncurl");
    await page.getByTestId("tool-backgroundable-tool-names").fill("cargo test\nrg");
    await page.getByTestId("tool-subagent-targets").fill("did:key:zDelegate:ops");
    await page.getByTestId("tool-cross-deployment-spawn-timeout").fill("45");
    await saveConfig(page, "tool-selection-save");

    await openConfigTab(page, "timerTriggers");
    await page.getByTestId("schedule-concurrency").selectOption("latest_only");
    await page.getByTestId("schedule-enabled").uncheck();
    await saveConfig(page, "schedule-save");

    await openConfigTab(page, "eventTriggers");
    await page.getByTestId("event-trigger-new").click();
    await page.getByTestId("event-trigger-id").fill("agent-request-created");
    await page.getByTestId("event-trigger-source-collection").fill("AgentRequest");
    await page.getByTestId("event-trigger-task-id").selectOption("host-check");
    await page.getByTestId("event-trigger-concurrency").selectOption("parallel");
    await page.getByTestId("event-trigger-filter").fill('{ "state": "Pending" }');
    await saveConfig(page, "event-trigger-save");
    await expect(
      page.getByTestId("config-event-trigger-agent-request-created"),
    ).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });

  test("defra_query allowlist edits persist and policy facts stay read-only", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigTab(page, "toolSelections");

    // Read-only policy facts render from the loaded selection; the write-tools
    // row decodes the WriteToolDecl JSON (friendly name, not a raw blob).
    await expect(page.getByTestId("tool-policy-version")).toHaveText("tool-policy/v1");
    await expect(page.getByTestId("tool-write-tools")).toContainText("upsert_note");
    await expect(page.getByTestId("tool-write-tools")).not.toContainText("tool_name");

    // Edit the read-scope allowlist and save.
    await page
      .getByTestId("tool-defra-query-collections")
      .fill("AgentRequest\nAgentResponse\nAgentSession");
    await saveConfig(page, "tool-selection-save");

    // Round-trip: navigate away and back — the edit persisted in the snapshot,
    // and preserve-on-absent kept the display-only facts across the save.
    await openConfigTab(page, "behavior");
    await openConfigTab(page, "toolSelections");
    await expect(page.getByTestId("tool-defra-query-collections")).toHaveValue(
      "AgentRequest\nAgentResponse\nAgentSession",
    );
    await expect(page.getByTestId("tool-policy-version")).toHaveText("tool-policy/v1");
    await expect(page.getByTestId("tool-write-tools")).toContainText("upsert_note");
    await expectNoPageHorizontalOverflow(page);
  });
});
