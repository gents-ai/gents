import { expect, gotoHarness, test } from "./desktopTest";

test("a compaction marks where it happened and opens what the agent kept", async ({
  page,
}) => {
  await gotoHarness(page, "days");
  await page.getByTestId("session-session-intro").click();
  const marker = page.getByTestId("session-compaction-event");
  await expect(marker).toContainText("Compacted");
  /* the runtime counts what the model was sent, so no count is shown */
  await expect(marker).not.toContainText("128 messages");
  await expect(marker).toContainText("182k → 11.9k tokens (−93%)");
  const toggle = marker.getByRole("button", { name: /Compacted/ });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByTestId("compaction-summary")).toHaveCount(0);

  /* the whole middle of the marker opens it, not only its label */
  await marker.getByText("182k → 11.9k tokens").click();
  const summary = page.getByRole("region", { name: "Compaction summary" });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  /* the goal and next actions first, the rest behind one button */
  await expect(summary.getByRole("term")).toHaveText(["Goal", "Next"]);
  await expect(summary).toContainText("Fix the flaky parser test");
  await expect(summary.getByRole("listitem")).toHaveText([
    "Add a regression test for an empty header.",
  ]);
  await expect(summary).not.toContainText("Continuation checkpoint");

  await summary.getByRole("button", { name: "Show 3 more" }).click();
  await expect(summary.getByRole("term")).toHaveText([
    "Goal",
    "Done",
    "Next",
    "Read",
    "Changed",
  ]);
  await expect(summary.locator("code")).toHaveText(["parse_header"]);
  /* the entry's file lists stand in for the ones written into the summary */
  await expect(
    summary.getByRole("list", { name: "Files read" }).getByRole("listitem"),
  ).toHaveText(["src/parser.rs", "tests/parser.rs"]);
  await expect(
    summary.getByRole("list", { name: "Files modified" }).getByRole("listitem"),
  ).toHaveText(["src/parser.rs"]);
  await summary.getByRole("button", { name: "Show less" }).click();
  await expect(summary.getByRole("term")).toHaveText(["Goal", "Next"]);

  await toggle.click();
  await expect(page.getByTestId("compaction-summary")).toHaveCount(0);
});
