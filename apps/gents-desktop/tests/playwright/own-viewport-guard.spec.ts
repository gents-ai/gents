import { gotoHarness, OWN_VIEWPORT, test } from "./desktopTest";

/* each of these breaks the rule an OWN_VIEWPORT test keeps; the guard in
   desktopTest.ts must fail them, so a passing one means the guard is gone */
test.describe("the own-viewport guard", { tag: OWN_VIEWPORT }, () => {
  test("fails a tagged test that draws before setting its own window size", async ({
    page,
  }) => {
    test.fail();
    await gotoHarness(page);
  });

  test("fails a tagged test that reads the window size before setting its own", async ({
    page,
  }) => {
    test.fail();
    page.viewportSize();
  });
});

test("fails a test tagged in its title that draws before setting its own window size @own-viewport", async ({
  page,
}) => {
  test.fail();
  await gotoHarness(page);
});
