/* The plugin frame in a real engine — Chromium and WebKit — through the
   real pipeline: the sandboxed iframe, the document the shell composes
   with the runtime inline, the port. The in-process vitest fixture
   cannot say whether an opaque origin can load the runtime; this does. */
import type { FrameLocator } from "@playwright/test";

import { expect, test, type Page } from "./desktopTest";

type Records = Record<string, { status: string; error?: string }>;

async function gotoSandboxHarness(page: Page) {
  await page.goto("/tests/ui-harness/sandbox.html");
  await page.waitForFunction(() => Boolean(window.__UX_HARNESS__));
}

async function loadProbe(page: Page) {
  const id = await page.evaluate(() => window.__UX_HARNESS__.load());
  expect(id).toBe("probe");
  await expect
    .poll(
      async () =>
        (await page.evaluate(() => window.__UX_HARNESS__.records())) as Records,
      { timeout: 10_000 },
    )
    .toMatchObject({ probe: { status: "loaded" } });
}

const frameOf = (page: Page): FrameLocator =>
  page.frameLocator('iframe[data-ux-frame="harness/probe"]');

test.describe("the plugin frame", () => {
  test.beforeEach(async ({ page }) => {
    await gotoSandboxHarness(page);
  });

  test("loads its runtime in a sandboxed frame and registers within the deadline", async ({
    page,
  }) => {
    await loadProbe(page);
    const iframe = page.locator('iframe[data-ux-frame="harness/probe"]');
    await expect(iframe).toHaveAttribute("sandbox", "allow-scripts");
    await expect(iframe).toHaveAttribute("srcdoc", /Content-Security-Policy/);
    expect(await iframe.getAttribute("src")).toBeNull();
    /* the shell-drawn rows arrived as data */
    await expect(page.getByTestId("ux-header-action")).toHaveText(/probe/);
    await expect(page.getByTestId("ux-directive-chip")).toHaveText("Probe 7");
  });

  test("has an opaque origin, no IPC, and a policy that reaches no network", async ({
    page,
    browserLogs,
  }) => {
    await loadProbe(page);
    const frame = frameOf(page);
    /* the page draws inside the frame and reports what it sees there */
    await expect(frame.getByTestId("origin")).toHaveText("null");
    await expect(frame.getByTestId("ipc")).toHaveText("undefined");
    const pageFrame = page.frames().find((f) => f.parentFrame() === page.mainFrame());
    expect(pageFrame).toBeTruthy();
    const probes = await pageFrame!.evaluate(async () => {
      const out: Record<string, string> = {};
      try {
        await fetch("/tests/ui-harness/sandbox.html");
        out.fetch = "allowed";
      } catch (e) {
        out.fetch = "blocked";
      }
      try {
        /* a script element: the policy, not the bundler, is what refuses it */
        await new Promise<void>((resolve, reject) => {
          const el = document.createElement("script");
          el.src = "https://example.invalid/x.js";
          el.onload = () => resolve();
          el.onerror = () => reject(new Error("refused"));
          document.head.appendChild(el);
        });
        out.script = "allowed";
      } catch (e) {
        out.script = "blocked";
      }
      try {
        // eslint-disable-next-line no-eval
        eval("1");
        out.eval = "allowed";
      } catch (e) {
        out.eval = "blocked";
      }
      try {
        void window.parent.document;
        out.parent = "reachable";
      } catch (e) {
        out.parent = "blocked";
      }
      out.tauri = typeof (window as { __TAURI_INTERNALS__?: unknown })
        .__TAURI_INTERNALS__;
      return out;
    });
    expect(probes).toEqual({
      fetch: "blocked",
      script: "blocked",
      eval: "blocked",
      parent: "blocked",
      tauri: "undefined",
    });
    /* each refusal is the policy's, reported as such; the fixture treats
       console errors as failures, so the ones this test provoked are
       accounted for here and taken out */
    const refusals = browserLogs.filter(
      (line) =>
        line.startsWith("[console:error]") &&
        /Content Security Policy|connect-src|script-src/.test(line),
    );
    expect(refusals.some((line) => /connect-src/.test(line))).toBe(true);
    expect(refusals.some((line) => /script-src|example\.invalid/.test(line))).toBe(
      true,
    );
    for (const line of refusals) browserLogs.splice(browserLogs.indexOf(line), 1);
  });

  test("renders the page in the frame, keeps its state across a shell re-render, and answers verbs under its own identity", async ({
    page,
  }) => {
    await loadProbe(page);
    const frame = frameOf(page);
    await expect(frame.getByTestId("plugin-page")).toBeVisible();
    /* the data verb ran through the host with the listing and node filled in */
    await expect(frame.getByTestId("rows")).toHaveText("from the host");
    const calls = await page.evaluate(() => window.__UX_HARNESS__.calls());
    expect(calls).toContainEqual({
      verb: "data.call",
      args: ["harness/probe", "did:key:harness", "board", "list_tasks", {}],
    });
    /* a draft survives the shell re-rendering around the viewport */
    await frame.getByTestId("draft").fill("half written");
    await page.evaluate(() => window.__UX_HARNESS__.rerender());
    await expect(page.locator("main")).toHaveAttribute("data-generation", "1");
    await expect(frame.getByTestId("draft")).toHaveValue("half written");
    /* a click in the frame reaches the shell as a verb */
    await frame.getByTestId("navigate").click();
    await expect
      .poll(() => page.evaluate(() => window.__UX_HARNESS__.calls()))
      .toContainEqual({
        verb: "navigate",
        args: [{ name: "plugin", pluginId: "probe" }],
      });
    /* a click on a shell-drawn row reaches the frame as a handle call */
    await page.getByTestId("ux-header-action").click();
    await expect
      .poll(() =>
        page.evaluate(() => window.__UX_HARNESS__.storage("gents.ux.probe.clicked")),
      )
      .toBe("true");
    await page.getByTestId("ux-directive-chip").click();
    await expect
      .poll(() =>
        page.evaluate(() => window.__UX_HARNESS__.storage("gents.ux.probe.chip")),
      )
      .toBe('"7"');
  });

  test("takes no space until a surface shows it, and is placed over the viewport that does", async ({
    page,
  }) => {
    /* the frame exists from the first line of load(); until a page is on
       screen it must paint nothing and cover nothing */
    const fresh = await page.evaluate(() => {
      void window.__UX_HARNESS__.load();
      const frame = document.querySelector<HTMLIFrameElement>(
        'iframe[data-ux-frame="harness/probe"]',
      )!;
      const rect = frame.getBoundingClientRect();
      return {
        width: rect.width,
        height: rect.height,
        visibility: getComputedStyle(frame).visibility,
      };
    });
    expect(fresh).toEqual({ width: 0, height: 0, visibility: "hidden" });
    await expect(frameOf(page).getByTestId("plugin-page")).toBeVisible();
    const placed = await page.evaluate(() => {
      const frame = document.querySelector<HTMLIFrameElement>(
        'iframe[data-ux-frame="harness/probe"]',
      )!;
      const viewport = document.querySelector('[data-testid="ux-frame-viewport"]')!;
      const f = frame.getBoundingClientRect();
      const v = viewport.getBoundingClientRect();
      return {
        visibility: getComputedStyle(frame).visibility,
        overViewport:
          Math.abs(f.left - v.left) < 1 &&
          Math.abs(f.top - v.top) < 1 &&
          f.width > 0 &&
          Math.abs(f.height - v.height) < 1,
      };
    });
    expect(placed).toEqual({ visibility: "visible", overViewport: true });
  });

  for (const layout of ["block", "flex"] as const) {
    test(`fills a ${layout} page parent with a visible frame`, async ({ page }) => {
      await loadProbe(page);
      await page.getByTestId("page-viewport").evaluate((element, display) => {
        element.style.display = display;
        element.style.flexDirection = "column";
      }, layout);
      await expect
        .poll(
          async () =>
            (await page.getByTestId("ux-frame-viewport").boundingBox())?.height,
        )
        .toBe(320);
      await expect
        .poll(
          async () =>
            (await page.locator('iframe[data-ux-frame="harness/probe"]').boundingBox())
              ?.height,
        )
        .toBe(320);
      await expect(frameOf(page).getByTestId("plugin-page")).toBeVisible();
    });
  }

  test("takes the frame down with a disable and builds a new one on enable", async ({
    page,
  }) => {
    await loadProbe(page);
    await page.evaluate(() => window.__UX_HARNESS__.enable(false));
    await expect(page.locator('iframe[data-ux-frame="harness/probe"]')).toHaveCount(0);
    await expect(page.getByTestId("ux-header-action")).toHaveCount(0);
    await page.evaluate(() => window.__UX_HARNESS__.enable(true));
    await expect
      .poll(
        async () =>
          (await page.evaluate(() => window.__UX_HARNESS__.records())) as Records,
        { timeout: 10_000 },
      )
      .toMatchObject({ probe: { status: "loaded" } });
    await expect(page.locator('iframe[data-ux-frame="harness/probe"]')).toHaveCount(1);
    await expect(frameOf(page).getByTestId("plugin-page")).toBeVisible();
  });
});
