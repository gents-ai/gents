import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  BehaviorEnvironmentView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { SessionListSection } from "../src/components/sidebar-widgets/SessionListSection";

const AGENT = "did:key:z6MkAgent";

function session(overrides: Partial<SessionSummary>): SessionSummary {
  return {
    sessionId: "s-1",
    title: "release planning",
    previewText: "let's cut v2",
    messageCount: 3,
    toolCallCount: 0,
    updatedAt: "2026-07-17T10:00:00Z",
    ...overrides,
  } as SessionSummary;
}

function environment(
  overrides: Partial<BehaviorEnvironmentView> = {},
): BehaviorEnvironmentView {
  return {
    behaviorId: "default",
    displayName: "Amy",
    enabled: true,
    isDefault: true,
    modelName: "gpt-5",
    inferenceProfileName: "Default",
    workspaceRoot: "/work/amygdala",
    fileAccess: "read-write",
    bashAccess: "unrestricted",
    networkAccess: "enabled",
    skillNames: [],
    sessionCount: 2,
    activeSessionCount: 0,
    ...overrides,
  };
}

function renderList(
  sessions: SessionSummary[],
  overrides: {
    environments?: BehaviorEnvironmentView[];
    onCreateSession?: () => void;
    onOpenSession?: (sessionId: string) => void;
  } = {},
) {
  const onCreateSession = overrides.onCreateSession ?? vi.fn();
  const onOpenSession = overrides.onOpenSession ?? vi.fn();
  render(
    <SessionListSection
      sessions={sessions}
      environments={
        overrides.environments ?? [
          environment(),
          environment({
            behaviorId: "review",
            displayName: "Review",
            isDefault: false,
            workspaceRoot: "/work/reviews",
          }),
        ]
      }
      selectedAgentDid={AGENT}
      selectedSessionId={null}
      onSelectSession={vi.fn()}
      onOpenSession={onOpenSession}
      onCreateSession={onCreateSession}
    />,
  );
  return { onCreateSession, onOpenSession };
}

describe("session list", () => {
  it("searches titles, previews, and behavior environments", () => {
    renderList([
      session({ sessionId: "s-1", title: "release planning" }),
      session({
        sessionId: "s-2",
        behaviorId: "review",
        title: "standup",
        previewText: "deploy notes",
      }),
    ]);

    fireEvent.change(screen.getByTestId("session-search"), {
      target: { value: "review" },
    });
    expect(screen.queryByTestId("session-s-1")).not.toBeInTheDocument();
    expect(screen.getByTestId("session-s-2")).toBeInTheDocument();

    fireEvent.change(screen.getByTestId("session-search"), {
      target: { value: "zzz" },
    });
    expect(screen.getByText("No sessions match the search.")).toBeInTheDocument();
  });

  it("shows every environment in one session list", () => {
    renderList([
      session({ sessionId: "s-default", behaviorId: "default" }),
      session({
        sessionId: "s-unassigned",
        behaviorId: null,
        title: "unassigned chat",
      }),
      session({ sessionId: "s-review", behaviorId: "review", title: "review chat" }),
    ]);

    expect(screen.getByTestId("session-s-default")).toHaveTextContent("Amy");
    expect(screen.getByTestId("session-s-unassigned")).toHaveTextContent(
      "Unassigned behavior",
    );
    expect(screen.getByTestId("session-s-review")).toHaveTextContent(
      "Review · reviews",
    );
  });

  it("prioritizes lifecycle states without inventing client state", () => {
    renderList([
      session({ sessionId: "s-failed", title: "failed", turnState: "failed" }),
      session({ sessionId: "s-running", title: "running", turnState: "processing" }),
      session({ sessionId: "s-done", title: "done", turnState: "completed" }),
    ]);

    const headings = screen.getAllByRole("heading", { level: 3 });
    expect(headings.map((heading) => heading.textContent)).toEqual([
      "Needs attention",
      "Active",
      "Recent",
    ]);
  });

  it("shows relative time, preview, and task context", () => {
    renderList([
      session({
        updatedAt: new Date(Date.now() - 7_200_000).toISOString(),
        taskId: "task-a",
        taskName: "Daily review",
      }),
    ]);

    expect(screen.getByText("2h ago")).toBeInTheDocument();
    expect(screen.getByText("let's cut v2")).toBeInTheDocument();
    expect(screen.getByText("Daily review")).toBeInTheDocument();
  });

  it("opens sessions and sends new-session intent to the behavior catalog", () => {
    const { onCreateSession, onOpenSession } = renderList([
      session({ sessionId: "s-1" }),
    ]);

    fireEvent.click(screen.getByTestId("session-s-1"));
    expect(onOpenSession).toHaveBeenCalledWith("s-1");

    fireEvent.click(screen.getByTestId("session-new"));
    expect(onCreateSession).toHaveBeenCalledOnce();
  });
});
