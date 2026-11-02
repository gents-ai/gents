import { composer, expect, gotoHarness, openChat, test } from "./desktopTest";

/* a message is often code, names or commands: the system mustn't rewrite or
   predict it as it is typed */
test("the composer asks the system not to autocorrect, capitalise or predict", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  const box = composer(page);
  await expect(box).toHaveAttribute("autocorrect", "off");
  await expect(box).toHaveAttribute("autocapitalize", "off");
  await expect(box).toHaveAttribute("writingsuggestions", "false");
});
