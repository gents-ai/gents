import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  test,
} from "./desktopTest";

test.describe("desktop operations drawer rich states", () => {
  test("backgrounded tools expose filters, diagnostics, and row actions", async ({
    page,
  }) => {
    await gotoHarness(page, "operations-rich");
    await openChat(page);
    await page.getByRole("button", { name: /open operations drawer/i }).click();

    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await expect(
      page.getByRole("gridcell", { name: "cargo test", exact: true }),
    ).toBeVisible();
    await expect(page.getByText("query_logs")).toBeVisible();
    await expect(page.getByTestId("ops-live-count")).toContainText("2 backgrounded");

    await page.getByRole("button", { name: "Stuck" }).click();
    await expect(
      page.getByRole("gridcell", { name: "cargo test", exact: true }),
    ).toBeVisible();
    await expect(page.getByText("query_logs")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: /Open lineage for cargo test/ }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: /Interrupt parent request request-intro/ }),
    ).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });

  test("lineage, backend health, and MCP health handle populated operator data", async ({
    page,
  }) => {
    await gotoHarness(page, "operations-rich");
    await openChat(page);
    await page.getByRole("button", { name: /open operations drawer/i }).click();

    await page.getByRole("tab", { name: /Lineage/ }).click();
    await expect(page.getByRole("tree", { name: "Subagent lineage" })).toBeVisible();
    await expect(page.locator('[title="request-child-1"]').first()).toBeVisible();
    await page.getByLabel("Live only").check();
    await expect(page.locator('[title="request-child-1"]').first()).toBeVisible();

    await page.getByRole("tab", { name: /Backends/ }).click();
    await page.getByRole("button", { name: /OpenAI Harness/ }).click();
    await expect(page.getByText("Admission policy & probe")).toBeVisible();
    await expect(page.getByText("last call failed")).toBeVisible();
    await expect(page.getByRole("cell", { name: "rate_limited" })).toBeVisible();
    await expect(page.getByText("Local Ollama Harness")).toBeVisible();

    await page.getByRole("tab", { name: /MCP health/ }).click();
    await expect(page.getByText("mcp-observability")).toBeVisible();
    await expect(page.getByText("mcp-logs")).toBeVisible();
    await page.getByRole("button", { name: /Unhealthy/ }).click();
    await expect(page.getByText("mcp-observability")).toHaveCount(0);
    await expect(page.getByText("mcp-logs")).toBeVisible();
    await page.getByTestId("mcp-health-row-mcp-logs").click();
    await expect(page.getByText("connection refused", { exact: true })).toBeVisible();
    await page.getByTestId("mcp-health-probe-mcp-logs").click();
    await expect(page.getByTestId("mcp-health-status-mcp-logs")).toContainText(
      /stuck/i,
    );
    await expectNoPageHorizontalOverflow(page);
  });

  test("holds tab lists a parked call, badges the count, and resolves it", async ({
    page,
  }) => {
    await gotoHarness(page, "operations-rich");
    await openChat(page);
    await page.getByRole("button", { name: /open operations drawer/i }).click();

    const holdsTab = page.getByRole("tab", { name: /Holds/ });
    await expect(holdsTab).toContainText("1");
    await holdsTab.click();

    await expect(page.getByTestId("holds-panel")).toBeVisible();
    const row = page.getByTestId("hold-row-held-call-1");
    await expect(row).toBeVisible();
    await expect(row).toContainText("bash_unrestricted");
    await expect(row).toContainText("cargo publish");

    await page.getByTestId("hold-approve-held-call-1").click();
    await expect(page.getByTestId("holds-empty")).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });
});
