import { describe, expect, it, vi } from "vitest";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createDesktopShellConfigActions } from "../src/hooks/desktopShellConfigActions";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("config action selection boundary", () => {
  it.each(["save", "delete"] as const)(
    "does not invoke legacy selection authority when a deferred %s completes",
    async (operation) => {
      const pending = deferred<Record<string, never>>();
      const setSelectedAgentDid = vi.fn();
      const setSelectedBehaviorId = vi.fn();
      const api = {
        saveAgentConfig: vi.fn(() => pending.promise),
        deleteSkillConfig: vi.fn(() => pending.promise),
      } as unknown as DesktopApiAdapter;
      const actions = createDesktopShellConfigActions({
        api,
        mutateSnapshot: async <T,>(mutation: () => Promise<T>) => mutation(),
        setError: vi.fn(),
        setSavingBehaviorConfig: vi.fn(),
        setSavingConfig: vi.fn(),
        // These were formerly part of the factory API. Runtime sentinels prove
        // completion has no hidden path back into route selection.
        setSelectedAgentDid,
        setSelectedBehaviorId,
      } as Parameters<typeof createDesktopShellConfigActions>[0] & {
        setSelectedAgentDid: typeof setSelectedAgentDid;
        setSelectedBehaviorId: typeof setSelectedBehaviorId;
      });

      const completion =
        operation === "save"
          ? actions.onSaveAgentConfig({
              document: {
                agent_did: "agent-a",
                default_behavior_id: "behavior-a",
              },
            } as never)
          : actions.onDeleteSkillConfig({
              agentDid: "agent-a",
              skillId: "skill-a",
            });
      pending.resolve({});
      await completion;

      expect(setSelectedAgentDid).not.toHaveBeenCalled();
      expect(setSelectedBehaviorId).not.toHaveBeenCalled();
    },
  );
});
