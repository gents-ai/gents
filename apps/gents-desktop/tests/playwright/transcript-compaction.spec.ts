import { expect, gotoHarness, test } from "./desktopTest";

test("a compaction marks where it happened and opens what the agent kept", async ({
  page,
}) => {
  await gotoHarness(page, "days");
  await page.getByTestId("session-session-intro").click();
  const marker = page.getByTestId("session-compaction-event");
  await expect(marker).toContainText("Compacted 128 messages");
  await expect(marker).toContainText("182k → 11.9k tokens (−93%)");
  const toggle = marker.getByRole("button", { name: /Compacted 128 messages/ });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByTestId("compaction-summary")).toHaveCount(0);

  await toggle.click();
  const summary = page.getByRole("region", { name: "Compaction summary" });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(summary.getByRole("heading", { name: "Progress" })).toBeVisible();
  await expect(summary).toContainText("Fix the flaky parser test");
  await expect(
    summary.getByRole("list", { name: "Files read" }).getByRole("listitem"),
  ).toHaveText(["src/parser.rs", "tests/parser.rs"]);
  await expect(
    summary.getByRole("list", { name: "Files modified" }).getByRole("listitem"),
  ).toHaveText(["src/parser.rs"]);

  await toggle.click();
  await expect(page.getByTestId("compaction-summary")).toHaveCount(0);
});
