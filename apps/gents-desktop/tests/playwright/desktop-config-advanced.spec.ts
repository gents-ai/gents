import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openConfig,
  openConfigTab,
  openChatNavigation,
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
    await openChatNavigation(page);
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

    await openConfigTab(page, "backends");
    await page.getByTestId("backend-new").click();
    await expect(page.getByTestId("config-tab-backends")).toHaveClass(/selected/);
    await expect(page.getByTestId("backend-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("backend-id").fill("backend-created-from-behavior");
    await page.getByTestId("backend-name").fill("Created backend");
    await page.getByTestId("backend-endpoint").fill("http://localhost:11434/v1");
    await saveConfig(page, "backend-save");
    await expect(page.getByTestId("backend-models")).toHaveAttribute("readonly");

    await openConfigTab(page, "behavior");
    await page.getByTestId("behavior-create-profile").click();
    await expect(page.getByTestId("config-tab-profiles")).toHaveClass(/selected/);
    await expect(page.getByTestId("profile-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("profile-id").fill("profile-created-from-behavior");
    await page.getByTestId("profile-display-name").fill("Created profile");
    await page.getByTestId("profile-backend-id").fill("backend-created-from-behavior");
    await page.getByTestId("profile-model-name").fill("llama3.2");
    await saveConfig(page, "profile-save");

    await openConfigTab(page, "behavior");
    await page.getByTestId("behavior-create-tools").click();
    await expect(page.getByTestId("config-tab-tools")).toHaveClass(/selected/);
    await expect(page.getByTestId("tools-id")).not.toHaveAttribute("readonly");
    await page.getByTestId("tools-id").fill("tools-created-from-behavior");
    await page.getByTestId("tools-display-name").fill("Created tools");
    await saveConfig(page, "tools-save");
  });

  test("tool, schedule, and trigger editors preserve advanced controls", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);

    await openConfigTab(page, "tools");
    await page.getByTestId("tools-root").fill("/tmp/gents-bombadil/workspace/ops");
    await page.getByTestId("tools-files-mode").selectOption("ReadWrite");
    await saveConfig(page, "tools-save");

    await openConfigTab(page, "schedules");
    await page.getByTestId("schedule-new").click();
    await page.getByTestId("schedule-id").fill("host-check-cron");
    await page.getByTestId("schedule-cadence-kind").selectOption("cron");
    await page.getByTestId("schedule-cron-expression").fill("0 */6 * * *");
    await saveConfig(page, "schedule-save");
    await expect(page.getByTestId("config-schedule-host-check-cron")).toBeVisible();

    await openConfigTab(page, "eventSources");
    await page.getByTestId("event-source-new").click();
    await page.getByTestId("event-source-id").fill("agent-request-created");
    await page.getByTestId("event-source-source-collection").fill("AgentRequest");
    await page.getByTestId("event-source-filter").fill('{ "state": "Pending" }');
    await saveConfig(page, "event-source-save");
    await expect(
      page.getByTestId("config-event-source-agent-request-created"),
    ).toBeVisible();

    await openConfigTab(page, "triggers");
    await page.getByTestId("trigger-new").click();
    await page.getByTestId("trigger-id").fill("agent-request-created");
    await page.getByTestId("trigger-task-id").selectOption("host-check");
    await page.getByTestId("trigger-source-kind").selectOption("event");
    await page
      .getByTestId("trigger-source-event-source")
      .selectOption("agent-request-created");
    await page.getByTestId("trigger-concurrency").selectOption("latest_only");
    await saveConfig(page, "trigger-save");
    await expect(
      page.getByTestId("config-trigger-agent-request-created"),
    ).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });

  test("defra_query allowlist edits persist and policy facts stay read-only", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigTab(page, "tools");

    await page.getByTestId("tools-target-documents").fill("[]");

    await openConfigTab(page, "behavior");
    await openConfigTab(page, "tools");
    await expect(page.getByTestId("tools-target-documents")).toHaveValue("[]");
    await expectNoPageHorizontalOverflow(page);
  });
});
