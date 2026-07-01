import {
  expect,
  gotoHarness,
  openChat,
  openConfig,
  openConfigTab,
  PEER_ID,
  saveConfig,
  test,
} from "./desktopTest";

// Exercises the three harness scenarios that were defined but never driven:
// `loading`, `save-error`, `backend-health-error`. Assertions go deeper than the
// existing single-banner sad-path check — convergence, recovery, and failure
// isolation — so a regression in async/error handling can't ship unseen.
test.describe("desktop async states", () => {
  test("loading scenario converges from a busy fleet-empty to the dashboard", async ({
    page,
  }) => {
    // The `loading` scenario delays the snapshot fetch by 250ms; gotoHarness
    // resolves on the transient fleet-empty. The invariant we lock in is that a
    // slow-but-successful load converges to the full fleet with no error surfaced.
    await gotoHarness(page, "loading");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page.getByTestId(`fleet-row-${PEER_ID}`)).toBeVisible();
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });

  test("save-error surfaces the banner, suppresses the Saved chip, and recovers", async ({
    page,
  }) => {
    await gotoHarness(page, "save-error");
    await openConfig(page);
    await openConfigTab(page, "behavior");

    await page
      .getByTestId("behavior-system-prompt")
      .fill("This behavior save is rejected by the harness.");
    await page.getByTestId("behavior-save").click();

    // The failed save routes through the shell error banner (String(err) prefix).
    await expect(page.getByTestId("error-banner")).toContainText(
      "Harness rejected behavior save",
    );
    // Negative invariant: no "Saved" confirmation chip on a failed save.
    await expect(
      page.locator(".config-editor").getByText("Saved", { exact: true }),
    ).toHaveCount(0);
    // The button must recover, not stay stuck in the disabled "Saving..." state.
    const saveButton = page.getByTestId("behavior-save");
    await expect(saveButton).toBeEnabled();
    await expect(saveButton).toHaveText("Save Behavior");

    // `save-error` is scoped to behavior saves only: a backend save still succeeds
    // and clears the banner (setError(null) at the start of the next action).
    await openConfigTab(page, "backends");
    await page.getByTestId("backend-name").fill("OpenAI Harness Recovered");
    await saveConfig(page, "backend-save");
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });

  test("backend-health-error is isolated to the Backends tab; other ops tabs still work", async ({
    page,
  }) => {
    await gotoHarness(page, "backend-health-error");
    await openChat(page);

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();

    // Backends tab: the panel heading still renders, but the fetch shows an alert.
    await page.getByRole("tab", { name: /Backends/ }).click();
    await expect(page.getByRole("heading", { name: "Backend health" })).toBeVisible();
    await expect(page.locator(".backend-health__error")).toHaveText(
      "Failed to load backend health: Harness backend health bridge unavailable.",
    );

    // Isolation: the other operations tabs are unaffected by the backend failure.
    await page.getByRole("tab", { name: /MCP health/ }).click();
    await expect(
      page.getByRole("heading", { name: "MCP services / health" }),
    ).toBeVisible();
    await expect(page.getByText("mcp-observability")).toBeVisible();

    await page.getByRole("tab", { name: /Lineage/ }).click();
    await expect(page.getByRole("tree", { name: "Subagent lineage" })).toBeVisible();

    // The failure is panel-local — it never routes through the shell error banner.
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });
});
