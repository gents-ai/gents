import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatHeader } from "../src/components/chat/ChatHeader";
import { BackgroundedToolsPanel } from "../src/components/backgroundedTools";
import { useOperationsSnapshot } from "../src/components/backgroundedTools/useOperationsSnapshot";

vi.mock("../src/components/backgroundedTools/useOperationsSnapshot", async (orig) => ({
  ...(await orig()),
  useOperationsSnapshot: vi.fn(),
}));
const mockedSnapshot = vi.mocked(useOperationsSnapshot);

describe("session ops", () => {
  it("offers Fork only for a selected conversation and forwards the session id", () => {
    const onForkConversation = vi.fn();
    const { rerender } = render(
      <ChatHeader
        selectedSessionId="session-1"
        selectedConversationTitle="planning"
        behaviorLabel={null}
        runtimeHealth={null}
        renamingTitle={false}
        onRenameConversationTitle={vi.fn()}
        onForkConversation={onForkConversation}
      />,
    );
    fireEvent.click(screen.getByTestId("conversation-fork"));
    expect(onForkConversation).toHaveBeenCalledWith("session-1");

    rerender(
      <ChatHeader
        selectedSessionId={null}
        selectedConversationTitle={null}
        behaviorLabel={null}
        runtimeHealth={null}
        renamingTitle={false}
        onRenameConversationTitle={vi.fn()}
        onForkConversation={onForkConversation}
      />,
    );
    expect(screen.queryByTestId("conversation-fork")).not.toBeInTheDocument();
  });

  it("offers Resend on stuck diagnostics rows", async () => {
    mockedSnapshot.mockReturnValue({
      snapshot: {
        fetchedAt: new Date().toISOString(),
        backgroundedTools: [],
        stuckDiagnostics: [
          {
            requestId: "req-stale-1",
            severity: "critical",
            reason: "expiredProcessing",
          },
        ],
      },
      error: null,
      isLoading: false,
      refresh: vi.fn(),
    } as never);
    const onResendRequest = vi.fn();
    render(
      <BackgroundedToolsPanel agentDid="did:a" onResendRequest={onResendRequest} />,
    );

    await waitFor(() =>
      expect(screen.getByTestId("stuck-resend-req-stale-1")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByTestId("stuck-resend-req-stale-1"));
    expect(onResendRequest).toHaveBeenCalledWith("req-stale-1");
  });
});
