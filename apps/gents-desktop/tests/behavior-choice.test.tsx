import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { Shell } from "../src/ui/hooks/useShell";
import { useBehaviorChoice } from "../src/ui/screens/SessionScreen";

function shell(selectedSessionId: string | null, defaultBehaviorId: string): Shell {
  return {
    selectedSessionId,
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
