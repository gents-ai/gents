import { fireEvent, render, screen } from "@testing-library/react";
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
import { toolSummary } from "../src/ui/screens/tool-summary";
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

  it("opens the raw report from its line, and puts its actions after it", () => {
    render(
      <Notice
        tone="destructive"
        title="This turn couldn't finish"
        description="The provider was overloaded."
        details="provider error: 529 overloaded_error"
        actions={<button type="button">Retry</button>}
      />,
    );
    const line = screen.getByRole("button", {
      name: /The provider was overloaded\. Show details/,
    });
    expect(line).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByText("provider error: 529 overloaded_error")).not.toBeVisible();
    fireEvent.click(line);
    expect(line).toHaveAttribute("aria-expanded", "true");
    expect(line).toHaveAccessibleName(/Hide details/);
    expect(screen.getByText("provider error: 529 overloaded_error")).toBeVisible();
    expect(screen.queryByText("Details")).toBeNull();
    expect(screen.getByRole("button", { name: "Retry" })).toBeVisible();
  });

  it("draws its line as plain text when there's nothing to open", () => {
    render(<Notice title="Saved" description="All done." />);
    expect(screen.getByText("All done.").tagName).toBe("P");
    expect(screen.queryByRole("button")).toBeNull();
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

  it("lists a stopped command's particulars as rows, with no nested toggle", () => {
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
    expect(screen.getByText("Stopped by")).toBeVisible();
    expect(screen.getByText("a stop request on this request")).toBeVisible();
    expect(screen.getByText("interrupt request req-7f3a")).toBeVisible();
    expect(screen.queryByText("Details")).toBeNull();
    expect(screen.queryByText(/userCancelled/)).toBeNull();
    /* the step's own row says what happened */
    expect(toolSummary(tool).secondary).toBe("You stopped it.");
  });

  it("lists a refusal's particulars once, leaving its reason to the step's row", () => {
    const tool = {
      itemKey: "d",
      toolName: "bash",
      statusKind: "error",
      reconstruction: { state: "ready" },
      denial: {
        category: "network",
        categoryLabel: "Network",
        ruleId: "bash.network.deny_outbound",
        reasonLine: "Outbound network access is turned off.",
        deniedCommand: "curl",
        deniedArgument: "https://example.com",
        deniedSubcommand: null,
        diagnostic: "policy=tools-default",
      },
      presentation: {
        kind: "command",
        command: "curl https://example.com",
        exitCode: null,
        timedOut: false,
        failed: true,
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
    expect(screen.getByText("bash.network.deny_outbound")).toBeVisible();
    expect(screen.getByText("policy=tools-default")).toBeVisible();
    expect(screen.queryByText("Outbound network access is turned off.")).toBeNull();
    expect(toolSummary(tool).secondary).toBe("Outbound network access is turned off.");
  });
});
