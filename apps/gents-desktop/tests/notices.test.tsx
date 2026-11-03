import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { GoalView } from "@source-inc/gents-desktop-client";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client/generated/RenderedToolCallView";
import {
  cancelCauseText,
  errorWords,
  failureText,
  goalStatusText,
  reasonText,
  sentence,
  withoutCommandHints,
} from "../src/lib/reasonText";
import { Notice } from "../src/ui/components/Notice";
import { Goal } from "../src/ui/screens/SessionHeader";
import { ToolBody } from "../src/ui/screens/tool-views";

describe("a notice", () => {
  it("is a status for information and an alert for warnings and errors", () => {
    const { rerender } = render(<Notice title="Saved">Done.</Notice>);
    expect(screen.getByRole("status")).toHaveTextContent("Saved");
    rerender(<Notice tone="warning" title="Limit reached" />);
    expect(screen.getByRole("alert")).toHaveTextContent("Limit reached");
    rerender(<Notice tone="destructive" title="Couldn't finish" />);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't finish");
  });

  it("keeps the raw report under Details, and its actions after it", () => {
    render(
      <Notice
        tone="destructive"
        title="This turn couldn't finish"
        details="provider error: 529 overloaded_error"
        actions={<button type="button">Retry</button>}
      >
        <p>The provider was overloaded.</p>
      </Notice>,
    );
    const details = screen.getByText("Details").closest("details")!;
    expect(details).not.toHaveAttribute("open");
    expect(details).toHaveTextContent("provider error: 529 overloaded_error");
    expect(screen.getByRole("button", { name: "Retry" })).toBeVisible();
  });
});

describe("words for what the runtime reports", () => {
  it("makes a fragment a sentence", () => {
    expect(sentence("  the resume test   fails ")).toBe("The resume test fails.");
    expect(sentence("Done!")).toBe("Done!");
    expect(sentence("")).toBe("");
  });

  it("never offers a command to type", () => {
    expect(
      withoutCommandHints(
        "no other Claude account offers claude-sonnet-5-5; add one with `gents claude-login --label <label>`",
      ),
    ).toBe("no other Claude account offers claude-sonnet-5-5");
    expect(reasonText("sign in with `gents claude-login`")).toBe("");
  });

  it("reads a coded reason by its code, and an unknown one by its message", () => {
    expect(reasonText("verification_failed: the resume test still fails")).toBe(
      "The check of the work didn't pass: the resume test still fails.",
    );
    expect(reasonText("some_new_code: something happened")).toBe("Something happened.");
    expect(reasonText(null)).toBe("");
  });

  it("names recognised provider failures, and leaves the rest to Details", () => {
    expect(failureText("provider error: 529 overloaded_error")).toBe(
      "The provider was overloaded. Try again in a moment.",
    );
    expect(failureText("429 Too Many Requests")).toMatch(/limiting/);
    expect(failureText("prompt is too long for the context window")).toMatch(
      /too long/,
    );
    expect(failureText("an unusual failure")).toBeNull();
    expect(errorWords("setup catalog: connection refused (os error 61)")).toBe(
      "The node couldn't be reached. Check that it's running.",
    );
  });

  it("says a goal's status and why something stopped in words", () => {
    expect(goalStatusText("usage_limited")).toBe(
      "Stopped at the account's usage limit",
    );
    expect(goalStatusText("budget_limited")).toBe("Stopped at its token budget");
    expect(goalStatusText("waiting_on_review")).toBe("Waiting on review");
    expect(cancelCauseText("userCancelled")).toBe("You stopped it");
    expect(cancelCauseText("somethingElse")).toBe("It was stopped");
  });
});

describe("where the words are used", () => {
  it("draws a goal's status and reasons as words, not codes", () => {
    const goal: GoalView = {
      goalId: "g",
      objective: "Make the import resumable",
      status: "usage_limited",
      tokenBudget: null,
      tokensUsed: 0,
      activeTimeSeconds: 0,
      consecutiveBlockedAudits: 0,
      continuationSequence: 0,
      wrapupRequested: false,
      wrapupCompleted: false,
      lastBlockedReason: "verification_failed: the resume test still fails",
      lastFailure: "usage_limited: every Claude account has reached its usage limit",
      completionEvidence: null,
    };
    render(<Goal goal={goal} />);
    screen.getByRole("button", { name: "Show goal detail" }).click();
    return screen.findByText("Stopped at the account's usage limit").then(() => {
      expect(screen.queryByText(/usage_limited/)).toBeNull();
      expect(
        screen.getByText(
          "The check of the work didn't pass: the resume test still fails.",
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByText("The account reached its usage limit."),
      ).toBeInTheDocument();
    });
  });

  it("says a stopped command was stopped, with the cause's ids under Details", () => {
    const tool = {
      itemKey: "t",
      toolName: "bash",
      statusKind: "canceled",
      reconstruction: { state: "ready" },
      cancelCause: {
        cause: "userCancelled",
        source: "requestInterrupt",
        confidence: "authentic",
        at: null,
        evidence: ["interrupt request req-7f3a"],
      },
      presentation: {
        kind: "command",
        command: "npm test",
        exitCode: null,
        timedOut: false,
        failed: false,
        durationMs: null,
        cwd: null,
        executionMode: null,
        networkMode: null,
        stdout: "",
        stderr: "",
        fallbackOutput: null,
      },
    } as unknown as RenderedToolCallView;
    render(<ToolBody tool={tool} />);
    expect(screen.getByText("You stopped it.")).toBeVisible();
    const details = screen.getByText("Details").closest("details")!;
    expect(details).toHaveTextContent("a stop request on this request");
    expect(screen.queryByText(/userCancelled/)).toBeNull();
  });
});
