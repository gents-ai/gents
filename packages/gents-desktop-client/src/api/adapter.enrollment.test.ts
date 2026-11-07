import { describe, expect, it } from "vitest";

import { createMemoryTransport } from "../testing.js";
import { createDesktopApiAdapter } from "./adapter.js";

describe("enrollment input", () => {
  it("routes links and raw tokens to invitation enrollment and addresses to status enrollment", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_peer_enroll_invite: () => ({}),
        desktop_peer_enroll_status: () => ({}),
      },
    });
    const api = createDesktopApiAdapter(transport);
    const token = "a".repeat(150);
    await api.requestStatusEnrollment(` gents://invite/${token} `);
    await api.requestStatusEnrollment(token);
    await api.requestStatusEnrollment(" https://example.test:9191 ");

    expect(transport.calls).toEqual([
      {
        command: "desktop_peer_enroll_invite",
        args: { request: { invite: `gents://invite/${token}` } },
      },
      {
        command: "desktop_peer_enroll_invite",
        args: { request: { invite: token } },
      },
      {
        command: "desktop_peer_enroll_status",
        args: { request: { serverAddress: "https://example.test:9191" } },
      },
    ]);
  });
});
