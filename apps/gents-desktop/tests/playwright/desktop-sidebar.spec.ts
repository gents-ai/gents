import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openChatNavigation,
  openConfigTab,
  test,
} from "./desktopTest";

test.describe("desktop sidebar workflows", () => {
  test("connected peer actions, behavior switching, and new chats stay wired", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await openChatNavigation(page);

    const connectedPeer = page.locator(".connected-peer-card");
    await expect(connectedPeer).toContainText("Bombadil UI Agent");
    await expect(connectedPeer).toContainText("connected");

    await connectedPeer.getByRole("button", { name: "Fleet Dashboard" }).click();
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await openChat(page);
    await openChatNavigation(page);
    await connectedPeer.getByRole("button", { name: "Configure" }).click();
    await expect(page.locator(".config-workspace")).toBeVisible();
    await page.getByTestId("config-back-tab").click();
    await openChatNavigation(page);

    await page.getByTestId("sidebar-behavior-ops").click();
    await expect(page.getByTestId("sidebar-behavior-ops")).toHaveClass(/selected/);
    await expect(page.locator(".chat-status")).toContainText("Ops");

    await page.getByTestId("sidebar-new-chat-ops").click();
    await expect(
      page.getByRole("heading", { name: "Start a conversation" }),
    ).toBeVisible();
    await page.getByTestId("composer-input").fill("Ops behavior smoke check");
    await page.getByTestId("composer-send").click();
    await expect(page.getByTestId("transcript-panel")).toContainText(
      "Ops behavior smoke check",
    );
    await expect(page.locator(".chat-status")).toContainText("Ops");
    await expectNoPageHorizontalOverflow(page);
  });

  test("conversation task filter separates manual and task-created sessions", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await openChatNavigation(page);

    const taskFilter = page.getByTestId("conversation-task-filter");
    await expect(taskFilter).toBeVisible();

    await taskFilter.selectOption("__untasked__");
    await expect(page.getByTestId("conversation-session-intro")).toBeVisible();

    await taskFilter.selectOption("host-check");
    await expect(page.getByText("No conversations for this task.")).toBeVisible();

    await page
      .locator(".connected-peer-card")
      .getByRole("button", {
        name: "Configure",
      })
      .click();
    await openConfigTab(page, "tasks");
    await page.getByTestId("task-run").click();
    await expect(page.getByTestId("task-run-status")).toContainText("request-");

    await page.getByTestId("config-back-tab").click();
    await openChatNavigation(page);
    await taskFilter.selectOption("host-check");
    const conversationList = page.locator(".conversation-list");
    await expect(conversationList.getByText("Run task host-check")).toBeVisible();
    await expect(conversationList.getByText("Host check")).toBeVisible();

    await taskFilter.selectOption("__untasked__");
    await expect(conversationList.getByText("Run task host-check")).toHaveCount(0);
    await expect(page.getByTestId("conversation-session-intro")).toBeVisible();
  });
});
