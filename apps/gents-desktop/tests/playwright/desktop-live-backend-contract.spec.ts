import type { Route } from "@playwright/test";

import { createDesktopUiHarness } from "../ui-harness/desktopHarness";
import { expect, gotoLiveHarness, openChat, PEER_ID, test } from "./desktopTest";

test.describe("desktop live backend harness contract", () => {
  test("reports missing bridge URL as a handled shell error", async ({ page }) => {
    await gotoLiveHarness(page);
    await expect(page.getByTestId("error-banner")).toContainText(
      "Live desktop harness backend requires bridgeUrl",
    );
  });

  test("boots the React shell through the live bridge HTTP adapter", async ({
    page,
  }) => {
    const fixture = createDesktopUiHarness();
    const requests: string[] = [];

    await page.route("**/__live-bridge/**", async (route) => {
      requests.push(`${route.request().method()} ${bridgePath(route)}`);
      await fulfillBridgeRoute(route, fixture);
    });

    await gotoLiveHarness(page, "/__live-bridge");

    await expect(page.locator("html")).toHaveAttribute(
      "data-desktop-ui-harness-bridge-url",
      /\/__live-bridge$/,
    );
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page.getByTestId(`fleet-row-${PEER_ID}`)).toBeVisible();

    await openChat(page);
    await expect(page.getByTestId("transcript-panel")).toContainText(
      "desktop UI test agent",
    );
    expect(requests).toContain("GET /desktop/client/snapshot");
    expect(requests).toContain("POST /desktop/selected-agent");
    expect(requests).toContain("POST /desktop/session/snapshot");
  });
});

function bridgePath(route: Route) {
  const url = new URL(route.request().url());
  return url.pathname.replace(/^\/__live-bridge/, "") || "/";
}

async function fulfillBridgeRoute(
  route: Route,
  fixture: ReturnType<typeof createDesktopUiHarness>,
) {
  const method = route.request().method();
  const path = bridgePath(route);

  if (method === "GET" && path === "/desktop/version") {
    await fulfillJson(route, { version: 1 });
    return;
  }
  if (method === "GET" && path === "/desktop/client/snapshot") {
    await fulfillJson(route, await fixture.adapter.fetchDesktopSnapshot());
    return;
  }
  if (method === "POST" && path === "/desktop/client/start") {
    await fulfillJson(route, await fixture.adapter.fetchDesktopSnapshot());
    return;
  }
  if (method === "POST" && path === "/desktop/client/shutdown") {
    await fulfillJson(route, await fixture.adapter.fetchDesktopSnapshot());
    return;
  }
  if (method === "POST" && path === "/desktop/selected-agent") {
    await fulfillJson(route, {});
    return;
  }
  if (method === "POST" && path === "/desktop/session/snapshot") {
    const body = route.request().postDataJSON() as {
      sessionId: string;
      agentDid?: string | null;
      requestId?: string | null;
    };
    await fulfillJson(
      route,
      await fixture.adapter.fetchSessionSnapshot(
        body.sessionId,
        body.agentDid,
        body.requestId,
      ),
    );
    return;
  }
  if (method === "POST" && path === "/desktop/operations/snapshot") {
    await fulfillJson(
      route,
      await fixture.adapter.fetchOperationsSnapshot({ rootRequestId: null }),
    );
    return;
  }

  await route.fulfill({
    status: 404,
    contentType: "application/json",
    body: JSON.stringify({ error: `unhandled bridge route: ${method} ${path}` }),
  });
}

async function fulfillJson(route: Route, value: unknown) {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(value),
  });
}
