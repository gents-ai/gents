import { act, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { listedSession } from "../src/hooks/fleetStore";
import { SentText } from "../src/ui/screens/SkillChips";
import { node, publish, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const parsed = vi.hoisted(() => vi.fn());
vi.mock("@gents/ui/conversation", async (importOriginal) => {
  const kit = await importOriginal<typeof import("@gents/ui/conversation")>();
  return {
    ...kit,
    parseParts: (...args: Parameters<typeof kit.parseParts>) => {
      parsed();
      return kit.parseParts(...args);
    },
  };
});

const NODE = deployment.nodeDid;

const summary = (sessionId: string, previewText: string) => ({
  nodeDid: NODE,
  sessionId,
  requesterDid: null,
  title: sessionId,
  previewText,
  updatedAt: "2026-10-31T00:00:00Z",
});

describe("a sent message with a session reference", () => {
  it("does not read its text again when a session in the fleet changes", () => {
    const app = testApp({
      deployments: [
        node({ sessions: [summary("intro", "Hello"), summary("busy", "Step 1")] }),
      ],
    });
    const { container } = render(
      <SentText content="see [intro](gents:session/intro)" skills={[]} />,
      { wrapper: withApp(app) },
    );
    expect(container.querySelector('[data-slot="session-chip"]')).toHaveTextContent(
      "intro",
    );
    const reads = parsed.mock.calls.length;
    act(() =>
      publish(app, [
        node({ sessions: [summary("intro", "Hello"), summary("busy", "Step 2")] }),
      ]),
    );
    expect(listedSession(app.stores.fleet.getState(), NODE, "busy")?.previewText).toBe(
      "Step 2",
    );
    expect(parsed.mock.calls.length).toBe(reads);
  });
});
