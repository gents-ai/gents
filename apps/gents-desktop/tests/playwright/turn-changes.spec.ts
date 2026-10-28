import { readFileSync } from "node:fs";
import { expect, gotoHarness, seedLocalStorage, test } from "./desktopTest";

/* The webview enforces the app's CSP, which the harness server does not
   send; a highlighter the policy blocks would leave the panel blank in the
   app while passing here. The dev policy is the shipped one plus the dev
   server's inline scripts and address (pinned by csp.test.ts), so the
   harness gets it with that address pointed at this server. */
const devCsp: string = JSON.parse(
  readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
).app.security.devCsp;

/* a routed document counts as public to Chromium, which then blocks its
   socket to the dev server on the loopback address */
test.use({
  launchOptions: [
    async ({ browserName }, use) =>
      use(
        browserName === "chromium"
          ? { args: ["--disable-features=LocalNetworkAccessChecks"] }
          : {},
      ),
    { scope: "worker" },
  ],
});

test("a turn's changed files open their recorded diffs, highlighted under the app's CSP", async ({
  page,
  baseURL,
}) => {
  const origin = new URL(baseURL!);
  const csp = devCsp
    .replaceAll("ws://localhost:1420", `ws://${origin.host}`)
    .replaceAll("http://localhost:1420", origin.origin);
  await page.route("**/tests/ui-harness/harness.html*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: { ...response.headers(), "content-security-policy": csp },
    });
  });
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();

  const card = page.getByTestId("turn-changes");
  await expect(card).toContainText("2 files changed in src");
  await card.getByRole("button", { name: /lexer\.rs/ }).click();

  await expect(
    page.getByRole("button", { name: "Choose a file, lexer.rs of 2" }),
  ).toContainText("2 of 2");
  const diffs = page.locator("diffs-container");
  await expect(diffs.getByText("Ast::default", { exact: false })).toBeVisible();
  await expect(diffs.getByText("split_whitespace", { exact: false })).toBeVisible();
  await expect(page.getByText("Highlighting could not load")).toHaveCount(0);
  /* a diff shows plain first and takes its colours from the workers */
  await expect(
    diffs
      .first()
      .locator(
        "[data-line-type=change-addition][data-line] span[style*='--diffs-token']",
      )
      .first(),
  ).toBeAttached();
});

/* read by the harness's recorded-versions fetch */
const UNREACHABLE = "harness.fileChangesUnreachable";

test("files on a node that cannot be reached say so together, and come back together", async ({
  page,
}) => {
  await seedLocalStorage(page, { [UNREACHABLE]: "1" });
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();
  await page
    .getByTestId("turn-changes")
    .getByRole("button", { name: /parser\.rs/ })
    .click();

  /* the second file fails with the first rather than waiting its own turn */
  const reason = page.getByText("Reconnect this runtime to load new file versions.");
  await expect(reason).toHaveCount(2, { timeout: 2500 });

  await page.evaluate((key) => localStorage.removeItem(key), UNREACHABLE);
  await page.getByRole("button", { name: "Try again" }).first().click();
  const diffs = page.locator("diffs-container");
  await expect(diffs.getByText("Ast::default", { exact: false })).toBeVisible();
  await expect(diffs.getByText("split_whitespace", { exact: false })).toBeVisible();
});

test("the file list widens from its handle and returns to its own width", async ({
  page,
}) => {
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();
  await page
    .getByTestId("turn-changes")
    .getByRole("button", { name: /parser\.rs/ })
    .click();
  const handle = page.getByRole("separator", { name: "Resize file list" });
  test.skip(!(await handle.isVisible()), "the panel is too narrow for its file list");
  const list = page.getByRole("navigation", { name: "Files changed in this turn" });
  const width = async () => (await list.boundingBox())!.width;
  const was = await width();
  await handle.focus();
  await page.keyboard.press("ArrowLeft");
  await expect.poll(width).toBeCloseTo(was + 16, 0);
  await handle.dblclick();
  await expect.poll(width).toBeCloseTo(was, 0);
});

test("choosing a collapsed file opens it again", async ({ page }) => {
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();
  await page
    .getByTestId("turn-changes")
    .getByRole("button", { name: /parser\.rs/ })
    .click();
  const header = page
    .locator("section[data-path='src/parser.rs'] button[aria-expanded]")
    .first();
  await header.click();
  await expect(header).toHaveAttribute("aria-expanded", "false");
  await page.getByRole("button", { name: /^Choose a file/ }).click();
  await page.getByRole("menuitem", { name: /parser\.rs/ }).click();
  await expect(header).toHaveAttribute("aria-expanded", "true");
});

test("a file shows all its lines from its header, and hides the unchanged ones again", async ({
  page,
}) => {
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();
  await page
    .getByTestId("turn-changes")
    .getByRole("button", { name: /lexer\.rs/ })
    .click();
  const section = page.locator("section[data-path='src/lexer.rs']");
  const tail = section.getByText("fn rule_24", { exact: false });
  await expect(section.getByText("split_whitespace", { exact: false })).toBeVisible();
  await expect(tail).toHaveCount(0);

  await section.getByRole("button", { name: "Show all lines of lexer.rs" }).click();
  await expect(tail).toBeVisible();
  await section
    .getByRole("button", { name: "Hide unchanged lines in lexer.rs" })
    .click();
  await expect(tail).toHaveCount(0);
  await expect(section.getByText("split_whitespace", { exact: false })).toBeVisible();
});
