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
    await expect(page.getByText("cargo test")).toBeVisible();
    await expect(page.getByText("query_logs")).toBeVisible();
    await expect(page.getByText("2 live")).toBeVisible();

    await page.getByRole("button", { name: "Stuck" }).click();
    await expect(page.getByText("cargo test")).toBeVisible();
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
    await expect(page.getByText("request-child-1")).toBeVisible();
    await page.getByLabel("Live only").check();
    await expect(page.getByText("request-child-1")).toBeVisible();

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
});
