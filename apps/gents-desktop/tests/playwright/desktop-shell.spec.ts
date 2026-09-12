import {
  composer,
  enabledButtonsWithoutAccessibleNames,
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  primarySurfaceCount,
  test,
} from "./desktopTest";

test.describe("kit shell", () => {
  test("default harness lands on sessions with one primary surface", async ({
    page,
  }) => {
    await gotoHarness(page);
    await expect(page.getByTestId("app-shell")).toBeVisible();
    await expect(page.getByTestId("sessions-screen")).toBeVisible();
    await expect(primarySurfaceCount(page)).resolves.toBe(1);
    await expect(enabledButtonsWithoutAccessibleNames(page)).resolves.toEqual([]);
    await expectNoPageHorizontalOverflow(page);
  });

  test("new session opens the composer", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await expect(composer(page)).toBeEditable();
  });

  test("agents and configuration are reachable", async ({ page }) => {
    await gotoHarness(page);
    await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
    await expect(page.getByTestId("agents-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
    await expect(page.getByText("Network", { exact: true })).toHaveCount(0);

    await openConfig(page);
    await expect(page.getByText("Agent details")).toBeVisible();
  });

  test("session context details have an explicit close control", async ({ page }) => {
    await gotoHarness(page);
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();
    await page.getByTestId("context-meter").click();
    await expect(page.getByTestId("context-details")).toBeVisible();

    await page.getByRole("button", { name: "Close context details" }).click();
    await expect(page.getByTestId("context-details")).not.toBeVisible();
  });

  test("mailbox is reachable from the rail", async ({ page }) => {
    await gotoHarness(page);
    const mailbox = page.getByRole("link", { name: "Mailbox" });
    if (!(await mailbox.first().isVisible())) {
      await page.getByRole("button", { name: "Menu" }).click();
    }
    await page.getByRole("link", { name: "Mailbox" }).last().click();
    await expect(page.getByTestId("mailbox-screen")).toBeVisible();
  });

  test("empty fleet is the first-run setup", async ({ page }) => {
    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Let’s get set up" })).toBeVisible();
  });

  test("bridge failure stays on the startup retry screen", async ({ page }) => {
    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("startup-screen")).toBeVisible();
    await expect(page.getByTestId("startup-retry")).toBeVisible();
  });

  test("offline transport keeps the locally cached conversation visible", async ({
    page,
  }) => {
    await gotoHarness(page, "sync-offline");
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();

    await expect(page.getByTestId("session-screen")).toBeVisible();
    await expect(
      page.getByText(/seeded turn gives the transcript a stable row/),
    ).toBeVisible();
  });
});
