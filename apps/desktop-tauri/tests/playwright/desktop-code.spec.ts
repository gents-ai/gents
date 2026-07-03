import { expect, gotoHarness, openChat, test } from "./desktopTest";

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
    await expect(page.getByTestId("code-diff")).toContainText("Ast::default()");

    await expect(page.getByTestId("code-command")).toContainText("cargo test parser");
    await expect(page.getByTestId("code-exit")).toHaveText("exit 0");
    await expect(page.getByTestId("code-terminal")).toContainText("2 passed");
  });

  test("Code mode surfaces the agent's working directory and permission boundary", async ({
    page,
  }) => {
    await gotoHarness(page, "coding");
    await openChat(page);
    await page.getByTestId("sidebar-open-code").click();

    const header = page.getByTestId("code-context-header");
    await expect(header).toBeVisible();
    await expect(page.getByTestId("code-context-workdir")).toContainText(
      "/tmp/defra-agent-bombadil/workspace",
    );
    await expect(page.getByTestId("code-context-files")).toHaveText("read-only");
    // The code-aware diff renders inside Code mode as well.
    await expect(page.getByTestId("code-diff")).toBeVisible();

    // Back to Chat returns to the plain chat surface (header gone, composer live).
    await page.getByTestId("code-back-to-chat").click();
    await expect(page.getByTestId("code-context-header")).toHaveCount(0);
    await expect(page.getByTestId("composer-input")).toBeVisible();
  });
});
