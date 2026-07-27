import { expect, gotoHarness, openChat, openChatNavigation, test } from "./desktopTest";

test.describe("desktop code experience", () => {
  test("renders file edits as diffs and commands as terminal output", async ({
    page,
  }) => {
    await gotoHarness(page, "coding");
    await openChat(page);

    // Code-aware transcript rendering (applies in chat too): the agent's file
    // edit becomes a diff and its bash call becomes a terminal block.
    const fileEdit = page.getByTestId("code-file-edit");
    await expect(fileEdit).toContainText("src/parser.rs");
    await expect(fileEdit).not.toHaveAttribute("open", "");
    await fileEdit.locator("summary").click();
    await expect(page.getByTestId("code-diff")).toContainText("Ast::default()");

    const command = page.getByTestId("code-command");
    await expect(command).toContainText("cargo test parser");
    await expect(page.getByTestId("code-exit")).toHaveText("exit 0");
    await expect(command).not.toHaveAttribute("open", "");
    await command.locator("summary").click();
    await expect(page.getByTestId("code-terminal")).toContainText("2 passed");
  });

  test("Code mode surfaces the agent's working directory and permission boundary", async ({
    page,
  }) => {
    await gotoHarness(page, "coding");
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("sidebar-open-code").click();

    const header = page.getByTestId("code-context-header");
    await expect(header).toBeVisible();
    await expect(page.getByTestId("code-context-workdir")).toContainText(
      "/tmp/gents-bombadil/workspace",
    );
    await expect(page.getByTestId("code-context-files")).toHaveText("read-only");
    // The code-aware diff renders inside Code mode as well.
    await page.getByTestId("code-file-edit").locator("summary").click();
    await expect(page.getByTestId("code-diff")).toBeVisible();

    // Back to Chat returns to the plain chat surface (header gone, composer live).
    await page.getByTestId("code-back-to-chat").click();
    await expect(page.getByTestId("code-context-header")).toHaveCount(0);
    await expect(page.getByTestId("composer-input")).toBeVisible();
  });
});
