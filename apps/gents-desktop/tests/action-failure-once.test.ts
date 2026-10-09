import { describe, expect, it, vi } from "vitest";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

import { createConfigActions } from "../src/hooks/configActions";
import { wasShown } from "../src/hooks/actionFailure";
import { toastFailure } from "../src/ui/lib/failure";

/* #2043: a failed action is reported once. The action that ran it says what
   failed; a screen catching the same failure keeps its own state but does
   not report it again. */
describe("a failed action", () => {
  const actions = (saveSkillConfig: () => Promise<never>) => {
    const reportFailure = vi.fn();
    const created = createConfigActions({
      api: { saveSkillConfig } as unknown as DesktopApiAdapter,
      mutateSnapshot: async <T>(mutation: () => Promise<T>) => mutation(),
      reportFailure,
    });
    return { created, reportFailure };
  };

  it("is reported by its action, naming what failed, and not again by the screen", async () => {
    /* the bridge rejects with a plain string */
    const { created, reportFailure } = actions(() =>
      Promise.reject("skill name taken"),
    );
    const failure = await created
      .changeConfig("saveSkillConfig", {} as never)
      .catch((error: unknown) => error);

    expect(reportFailure).toHaveBeenLastCalledWith(
      "Couldn’t save the skill: skill name taken",
    );
    expect(failure).toBeInstanceOf(Error);
    expect((failure as Error).message).toBe("skill name taken");
    expect(wasShown(failure)).toBe(true);

    toastFailure("save", failure);
    expect(toast).not.toHaveBeenCalled();
  });

  it("that no action showed is reported by the screen", () => {
    toastFailure("start the task", new Error("args must be an object"));
    expect(toast).toHaveBeenCalledWith(
      "Couldn’t start the task: args must be an object",
    );
  });
});
