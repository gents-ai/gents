import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConversationListSection } from "../src/components/sidebar-widgets/ConversationListSection";
import type {
  ConversationSummary,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

const AGENT = "did:key:z6MkAgent";

function conv(overrides: Partial<ConversationSummary>): ConversationSummary {
  return {
    sessionId: "s-1",
    title: "release planning",
    previewText: "let's cut v2",
    messageCount: 3,
    toolCallCount: 0,
    updatedAt: "2026-07-17T10:00:00Z",
    ...overrides,
  } as ConversationSummary;
}

function renderList(
  conversations: ConversationSummary[],
  onRenameConversationTitle = vi.fn(),
) {
  render(
    <ConversationListSection
      conversations={conversations}
      deployments={[
        {
          agentDid: AGENT,
          label: "Agent",
          defaultBehaviorId: "default",
          behaviors: [
            { behaviorId: "default", displayName: "Amy", isDefault: true },
            { behaviorId: "session-classifier", displayName: "Session Classifier" },
          ],
          tasks: [],
        } as unknown as DeploymentView,
      ]}
      selectedAgentDid={AGENT}
      selectedBehaviorId="default"
      selectedSessionId={null}
      onSelectSession={vi.fn()}
      onRenameConversationTitle={onRenameConversationTitle}
    />,
  );
  return onRenameConversationTitle;
}

describe("conversation list", () => {
  it("filters by title and preview text", () => {
    renderList([
      conv({ sessionId: "s-1", title: "release planning" }),
      conv({ sessionId: "s-2", title: "standup", previewText: "deploy notes" }),
    ]);

    fireEvent.change(screen.getByTestId("conversation-search"), {
      target: { value: "deploy" },
    });
    expect(screen.queryByTestId("conversation-s-1")).not.toBeInTheDocument();
    expect(screen.getByTestId("conversation-s-2")).toBeInTheDocument();

    fireEvent.change(screen.getByTestId("conversation-search"), {
      target: { value: "zzz" },
    });
    expect(screen.getByText("No conversations match the search.")).toBeInTheDocument();
  });

  it("shows a relative timestamp with the raw value on hover", () => {
    renderList([conv({ updatedAt: new Date(Date.now() - 7_200_000).toISOString() })]);
    expect(screen.getByText("2h ago")).toBeInTheDocument();
  });

  it("shows only the selected behavior and keeps legacy sessions with the default", () => {
    renderList([
      conv({ sessionId: "s-default", behaviorId: "default" }),
      conv({ sessionId: "s-legacy", behaviorId: null, title: "legacy chat" }),
      conv({
        sessionId: "s-classifier",
        behaviorId: "session-classifier",
        title: "classification",
      }),
    ]);

    expect(screen.getByTestId("conversation-s-default")).toBeInTheDocument();
    expect(screen.getByTestId("conversation-s-legacy")).toBeInTheDocument();
    expect(screen.queryByTestId("conversation-s-classifier")).not.toBeInTheDocument();
  });

  it("renames inline and cancels on Escape", async () => {
    const onRename = renderList([conv({ sessionId: "s-1" })]);

    fireEvent.click(screen.getByTestId("conversation-rename-s-1"));
    const input = screen.getByTestId("conversation-rename-input-s-1");
    fireEvent.change(input, { target: { value: "v2 cutover" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRename).toHaveBeenCalledWith("s-1", "v2 cutover");

    fireEvent.click(await screen.findByTestId("conversation-rename-s-1"));
    fireEvent.keyDown(screen.getByTestId("conversation-rename-input-s-1"), {
      key: "Escape",
    });
    expect(onRename).toHaveBeenCalledTimes(1);
  });

  it("keeps a failed rename draft open and accessibly named", async () => {
    const onRename = vi.fn().mockRejectedValue(new Error("replica unavailable"));
    renderList([conv({ sessionId: "s-1" })], onRename);

    fireEvent.click(screen.getByTestId("conversation-rename-s-1"));
    const input = screen.getByTestId("conversation-rename-input-s-1");
    expect(input).toHaveAccessibleName("Rename release planning");
    fireEvent.change(input, { target: { value: "retry this title" } });
    fireEvent.blur(input);

    await waitFor(() =>
      expect(screen.getByTestId("conversation-rename-input-s-1")).toHaveValue(
        "retry this title",
      ),
    );
    expect(onRename).toHaveBeenCalledWith("s-1", "retry this title");
  });

  it("does not persist an unchanged display title", () => {
    const onRename = renderList([conv({ sessionId: "s-1" })]);

    fireEvent.click(screen.getByTestId("conversation-rename-s-1"));
    fireEvent.keyDown(screen.getByTestId("conversation-rename-input-s-1"), {
      key: "Enter",
    });

    expect(onRename).not.toHaveBeenCalled();
    expect(
      screen.queryByTestId("conversation-rename-input-s-1"),
    ).not.toBeInTheDocument();
  });
});
