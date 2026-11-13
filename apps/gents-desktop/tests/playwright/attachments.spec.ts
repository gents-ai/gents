import { expect, gotoHarness, openChat, test } from "./desktopTest";

const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aWQAAAABJRU5ErkJggg==",
  "base64",
);

test("picks images in the real composer, shows previews, and removes them", async ({
  page,
}, info) => {
  await gotoHarness(page);
  await openChat(page);
  await expect(page.getByRole("button", { name: "Attach files" })).toBeVisible();
  await page
    .getByLabel("Choose attachments")
    .setInputFiles({ name: "screenshot.png", mimeType: "image/png", buffer: png });
  await expect(page.getByTestId("attachment-tray")).toContainText("screenshot.png");
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeEnabled();
  await page.screenshot({
    animations: "disabled",
    path: info.outputPath("attachments-light.png"),
  });
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "dark";
  });
  await page.screenshot({
    animations: "disabled",
    path: info.outputPath("attachments-dark.png"),
  });
  await page.getByRole("button", { name: "Remove screenshot.png" }).click();
  await expect(page.getByTestId("attachment-tray")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeDisabled();
});

test("attaches a dropped file and keeps text when a file cannot be read", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  const data = await page.evaluateHandle((encoded) => {
    const transfer = new DataTransfer();
    transfer.items.add(
      new File(
        [Uint8Array.from(atob(encoded), (char) => char.charCodeAt(0))],
        "dropped.png",
        { type: "image/png" },
      ),
    );
    return transfer;
  }, png.toString("base64"));
  await page.getByTestId("composer").dispatchEvent("drop", { dataTransfer: data });
  await expect(page.getByTestId("attachment-tray")).toContainText("dropped.png");
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("Keep this draft");
  await page.getByLabel("Choose attachments").setInputFiles({
    name: "binary.dat",
    mimeType: "application/octet-stream",
    buffer: Buffer.from([0, 1, 2]),
  });
  await expect(page.getByRole("alert")).toContainText("Unsupported file");
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toHaveText(
    "Keep this draft",
  );
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeDisabled();
});

test("submits an image-only message and renders its retained attachment", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await page
    .getByLabel("Choose attachments")
    .setInputFiles({ name: "sent.png", mimeType: "image/png", buffer: png });
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByLabel("Sent attachments")).toBeVisible();
  await expect(page.getByLabel("Sent attachments")).toContainText("sent.png");
  await expect(page.getByTestId("attachment-tray")).toHaveCount(0);
});

test("keeps a saved send's attachment bytes in IndexedDB, past localStorage's quota", async ({
  page,
}) => {
  await gotoHarness(page);
  const kept = await page.evaluate(async (module) => {
    const { indexedDbSendAttachments } = (await import(
      /* @vite-ignore */ module
    )) as typeof import("../../src/hooks/sendAttachments");
    const store = indexedDbSendAttachments();

    const large = "QUFB".repeat(1_398_101) + "QQ==";
    await store.put("saved", [large, "bm90ZXM="]);
    await store.put("released", ["bm90ZXM="]);
    const read = await store.get("saved");
    await store.prune((key) => key === "saved");
    const pruned = [await store.get("saved"), await store.get("released")];
    await store.delete("saved");
    return {
      read: read?.map((data) => data.length),
      pruned: pruned.map((data) => data?.length ?? null),
      deleted: (await store.get("saved")) ?? null,
    };
  }, "/src/hooks/sendAttachments.ts");
  expect(kept).toEqual({
    read: [5_592_408, 8],
    pruned: [2, null],
    deleted: null,
  });
});
