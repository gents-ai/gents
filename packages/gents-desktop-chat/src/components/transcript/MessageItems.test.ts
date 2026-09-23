import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

import { AssistantMessageItem, UserMessageItem } from "./MessageItems.js";

type Assistant = Extract<RenderedTimelineItem, { kind: "assistantMessage" }>;
type User = Extract<RenderedTimelineItem, { kind: "userMessage" }>;

const assistant = (state: Assistant["reconstruction"]["state"]): Assistant => ({
  kind: "assistantMessage",
  itemKey: "header-physical-id",
  sequence: 1,
  content: null,
  reasoning: null,
  timestamp: null,
  reconstruction: { state },
});

describe("canonical message availability", () => {
  it("keeps an unresolved assistant header visible without inventing output", () => {
    const html = renderToStaticMarkup(createElement(AssistantMessageItem, {
      item: assistant("loading"),
    }));
    expect(html).toContain('data-testid="message-output-loading"');
    expect(html).toContain('role="status"');
    expect(html).not.toContain("message-copy");
  });

  it.each(["denied", "invalid"] as const)("renders %s distinctly and hides stale content", (state) => {
    const html = renderToStaticMarkup(createElement(AssistantMessageItem, {
      item: { ...assistant(state), content: "stale payload", reasoning: "stale reasoning" },
    }));
    expect(html).toContain(`data-testid="message-output-${state}"`);
    expect(html).toContain('role="alert"');
    expect(html).not.toContain("stale payload");
    expect(html).not.toContain("stale reasoning");
    expect(html).not.toContain("message-copy");
  });

  it("does not mistake a deliberately empty ready message for missing dependencies", () => {
    expect(renderToStaticMarkup(createElement(AssistantMessageItem, {
      item: assistant("ready"),
    }))).toBe("");
  });

  it("renders ready canonical content without a loading or integrity warning", () => {
    const html = renderToStaticMarkup(createElement(AssistantMessageItem, {
      item: { ...assistant("ready"), content: "Canonical answer" },
    }));
    expect(html).toContain("Canonical answer");
    expect(html).not.toContain("message-output-loading");
    expect(html).not.toContain("message-output-invalid");
  });

  it("also preserves unresolved user headers", () => {
    const item: User = {
      kind: "userMessage",
      itemKey: "user-header",
      requestId: "request",
      sequence: 0,
      content: null,
      timestamp: null,
      reconstruction: { state: "loading" },
    };
    const html = renderToStaticMarkup(createElement(UserMessageItem, { item }));
    expect(html).toContain('data-testid="message-output-loading"');
    expect(html).not.toContain("message-copy");
  });
});
