import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

import {
  AssistantMessageItem,
  PendingUserTurnItem,
  UserMessageItem,
} from "./MessageItems.js";

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
    const html = renderToStaticMarkup(
      createElement(AssistantMessageItem, {
        item: assistant("loading"),
      }),
    );
    expect(html).toContain('data-testid="message-output-loading"');
    expect(html).toContain('role="status"');
    expect(html).not.toContain("message-copy");
  });

  it.each(["denied", "invalid"] as const)(
    "renders %s distinctly and hides stale content",
    (state) => {
      const html = renderToStaticMarkup(
        createElement(AssistantMessageItem, {
          item: {
            ...assistant(state),
            content: "stale payload",
            reasoning: "stale reasoning",
          },
        }),
      );
      expect(html).toContain(`data-testid="message-output-${state}"`);
      expect(html).toContain('role="alert"');
      expect(html).not.toContain("stale payload");
      expect(html).not.toContain("stale reasoning");
      expect(html).not.toContain("message-copy");
    },
  );

  it("does not mistake a deliberately empty ready message for missing dependencies", () => {
    expect(
      renderToStaticMarkup(
        createElement(AssistantMessageItem, {
          item: assistant("ready"),
        }),
      ),
    ).toBe("");
  });

  it("renders ready canonical content without a loading or integrity warning", () => {
    const html = renderToStaticMarkup(
      createElement(AssistantMessageItem, {
        item: { ...assistant("ready"), content: "Canonical answer" },
      }),
    );
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

it("offers editing only for readable authored user messages", () => {
  const item: User = {
    kind: "userMessage",
    itemKey: "user",
    sequence: 0,
    ownsTurn: true,
    content: "A prompt",
    timestamp: null,
    reconstruction: { state: "ready" },
  };
  const onEdit = () => {};
  expect(
    renderToStaticMarkup(createElement(UserMessageItem, { item, onEdit })),
  ).toContain('aria-label="Edit message"');
  expect(
    renderToStaticMarkup(
      createElement(UserMessageItem, {
        item: { ...item, reconstruction: { state: "denied" } },
        onEdit,
      }),
    ),
  ).not.toContain('aria-label="Edit message"');
  expect(
    renderToStaticMarkup(
      createElement(UserMessageItem, {
        item: {
          ...item,
          kind: "automatedInput",
          origin: { kind: "backgroundCompletion" },
        },
        onEdit,
      }),
    ),
  ).not.toContain('aria-label="Edit message"');
});

describe("received session-message presentation", () => {
  const original =
    "[message agent:engineer session:parent node:sender · at:time · cause:cause]\n\nReview the layout.";
  const received = {
    kind: "automatedInput",
    itemKey: "received",
    requestId: "received-request",
    sequence: 1,
    timestamp: null,
    content: original,
    reconstruction: { state: "ready" },
    origin: {
      kind: "sessionMessage",
      senderNodeDid: "sender",
      senderSessionId: "parent",
      senderRequestId: "cause",
      displayBody: "Review the layout.",
    },
  } satisfies Extract<RenderedTimelineItem, { kind: "automatedInput" }>;
  it("keeps the native task body prominent and the original input in an explicit disclosure", () => {
    const html = renderToStaticMarkup(
      createElement(UserMessageItem, { item: received }),
    );
    expect(html).toContain('class="message-card received-card"');
    expect(html).not.toContain('class="message-card user-card"');
    expect(html).toContain('class="received-message-heading"');
    expect(html).toContain("Agent message");
    expect(html).toContain("<p>Review the layout.</p>");
    expect(html).toContain("<summary>Message details</summary>");
    expect(html.indexOf("[message agent:")).toBeGreaterThan(
      html.indexOf("<details"),
    );
    expect(html).not.toContain("<details open");
    expect(received.content).toBe(original);
  });
  it("does not guess the body from a routing-looking prefix without native display metadata", () => {
    const html = renderToStaticMarkup(
      createElement(UserMessageItem, {
        item: {
          ...received,
          origin: { ...received.origin, displayBody: undefined },
        },
      }),
    );
    expect(html).toContain("Task text unavailable");
    expect(html.indexOf("[message agent:")).toBeGreaterThan(
      html.indexOf("<details"),
    );
  });
  it.each(["loading", "denied", "invalid"] as const)(
    "keeps %s received content unavailable even when stale body metadata exists",
    (state) => {
      const html = renderToStaticMarkup(
        createElement(UserMessageItem, {
          item: { ...received, reconstruction: { state } },
        }),
      );
      expect(html).toContain(`data-testid="message-output-${state}"`);
      expect(html).not.toContain("Review the layout.");
      expect(html).not.toContain("[message agent:");
      expect(html).not.toContain("<details");
      expect(html).not.toContain("received-message-copy");
    },
  );
  it("leaves a person's literal routing-looking text in its authored bubble", () => {
    const html = renderToStaticMarkup(
      createElement(UserMessageItem, {
        item: {
          kind: "userMessage",
          itemKey: "literal",
          content: original,
          sequence: 1,
          timestamp: null,
          reconstruction: { state: "ready" },
        },
      }),
    );
    expect(html).toContain('class="message-card user-card"');
    expect(html).not.toContain("received-card");
    expect(html).toContain("[message agent:");
    expect(html).not.toContain("Message details");
  });
});

const goalMessage: User = {
  kind: "userMessage",
  itemKey: "goal-input",
  ownsTurn: true,
  sequence: 1,
  content: "Review the changes",
  timestamp: null,
  startedGoalId: "goal-1",
  reconstruction: { state: "ready" },
};

it("shows an applied opening marker while its transcript is still pending", () => {
  const item: Extract<RenderedTimelineItem, { kind: "pendingUserTurn" }> = {
    kind: "pendingUserTurn",
    itemKey: "pending-opening",
    requestId: "opening",
    content: "Review the changes",
    selectedSkillIds: [],
    lifecycleState: "pending",
    foldedIntoRequestId: null,
    origin: null,
    createdAt: null,
    startedGoalId: "goal-1",
  };
  expect(
    renderToStaticMarkup(createElement(PendingUserTurnItem, { item })),
  ).toContain("Sent as goal");
  expect(
    renderToStaticMarkup(
      createElement(PendingUserTurnItem, {
        item: { ...item, startedGoalId: undefined },
      }),
    ),
  ).not.toContain("Sent as goal");
});

it("preserves message actions alongside the saved goal attribution", () => {
  const html = renderToStaticMarkup(
    createElement(UserMessageItem, { item: goalMessage }),
  );
  expect(html).toContain("Sent as goal");
  expect(html).toContain("message-copy");
  expect(html).toContain("message-goal-attribution");
});

it.each(["loading", "denied", "invalid"] as const)(
  "hides the goal attribution for %s content",
  (state) => {
    const html = renderToStaticMarkup(
      createElement(UserMessageItem, {
        item: { ...goalMessage, reconstruction: { state } },
      }),
    );
    expect(html).not.toContain("Sent as goal");
  },
);

it("does not mark an ordinary follow-up as a goal", () => {
  const html = renderToStaticMarkup(
    createElement(UserMessageItem, {
      item: { ...goalMessage, startedGoalId: undefined },
    }),
  );
  expect(html).not.toContain("Sent as goal");
});
