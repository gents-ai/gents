import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { Shell } from "../src/ui/hooks/useShell";
import { useBehaviorChoice } from "../src/ui/screens/SessionScreen";

function shell(selectedSessionId: string | null, defaultBehaviorId: string): Shell {
  return {
    selectedSessionId,
    selectedBehaviorId: defaultBehaviorId,
    selectBehavior: vi.fn(),
    mailboxCause: null,
    selectedDeployment: {
      behaviors: [
        { behaviorId: "setup", isDefault: defaultBehaviorId === "setup" },
        { behaviorId: "coding", isDefault: defaultBehaviorId === "coding" },
      ],
    },
  } as Shell;
}

describe("new-session behavior choice", () => {
  it("renders only the shell decision and never retains a shadow selection", () => {
    const value = shell(null, "setup");
    const { result, rerender } = renderHook(({ value }) => useBehaviorChoice(value), {
      initialProps: { value },
    });
    act(() => result.current.setPicked("coding"));
    expect(value.selectBehavior).toHaveBeenCalledWith("coding");
    expect(result.current.behaviorId).toBe("setup");
    rerender({ value: { ...value, selectedBehaviorId: "coding" } });
    expect(result.current.behaviorId).toBe("coding");
    rerender({
      value: { ...value, selectedAgentDid: "other", selectedBehaviorId: null },
    });
    expect(result.current.behaviorId).toBeNull();
  });
  it("forgets the behavior explicitly picked for the previous session", async () => {
    const { result, rerender } = renderHook(({ value }) => useBehaviorChoice(value), {
      initialProps: { value: shell(null, "setup") },
    });

    act(() => result.current.setPicked("setup"));
    rerender({ value: shell("session-1", "setup") });
    rerender({ value: shell(null, "coding") });

    await waitFor(() => expect(result.current.behaviorId).toBe("coding"));
  });
});
