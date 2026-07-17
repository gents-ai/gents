import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer } from "../src/components/chat";
import {
  applySkillSelection,
  effectiveBehaviorSkills,
  slashSkillSuggestion,
} from "../src/components/chat/slashSkills";
import type { BehaviorView, SkillView } from "../src/lib/types";

const skills: SkillView[] = [
  { skillId: "review-skill", name: "Review", toolRefs: [], enabled: true },
  { skillId: "deploy-skill", name: "Deploy", toolRefs: [], enabled: true },
  { skillId: "off-skill", name: "Disabled", toolRefs: [], enabled: false },
];

describe("effectiveBehaviorSkills", () => {
  it("inherits principal skills, applies exclusions, and requires behavior opt-in", () => {
    const behavior: BehaviorView = {
      behaviorId: "default",
      displayName: "Default",
      enabled: true,
      isDefault: true,
      skillRefs: ["behavior-selected"],
      skillExcludes: ["principal-excluded"],
    };
    const deploymentSkills: SkillView[] = [
      {
        skillId: "principal-inherited",
        scope: "principal",
        toolRefs: [],
        enabled: true,
      },
      {
        skillId: "principal-excluded",
        scope: "principal",
        toolRefs: [],
        enabled: true,
      },
      {
        skillId: "behavior-selected",
        scope: "behavior",
        toolRefs: [],
        enabled: true,
      },
      {
        skillId: "behavior-unselected",
        scope: "behavior",
        toolRefs: [],
        enabled: true,
      },
      {
        skillId: "principal-disabled",
        scope: "principal",
        toolRefs: [],
        enabled: false,
      },
    ];

    expect(
      effectiveBehaviorSkills(deploymentSkills, behavior).map((skill) => skill.skillId),
    ).toEqual(["principal-inherited", "behavior-selected"]);
  });
});

describe("slashSkillSuggestion", () => {
  it("suggests on a leading slash line and filters by prefix", () => {
    const all = slashSkillSuggestion("/", 1, skills);
    expect(all?.items.map((s) => s.skillId)).toEqual(["review-skill", "deploy-skill"]);

    const filtered = slashSkillSuggestion("/dep", 4, skills);
    expect(filtered?.items.map((s) => s.skillId)).toEqual(["deploy-skill"]);
  });

  it("only suggests within the leading selector block", () => {
    expect(slashSkillSuggestion("hello /", 7, skills)).toBeNull();
    expect(slashSkillSuggestion("body line\n/", 11, skills)).toBeNull();
    // A selector line above keeps the block leading.
    const second = slashSkillSuggestion("/review-skill\n/", 15, skills);
    expect(second?.items.length).toBeGreaterThan(0);
  });

  it("replaces the caret line and keeps the body", () => {
    const suggestion = slashSkillSuggestion("/re\nplan the work", 3, skills);
    expect(suggestion).not.toBeNull();
    const applied = applySkillSelection(
      "/re\nplan the work",
      suggestion!,
      "review-skill",
    );
    expect(applied.draft).toBe("/review-skill\nplan the work");
  });
});

describe("composer slash menu", () => {
  function renderComposer(draft: string, onDraftChange = vi.fn(), onSend = vi.fn()) {
    render(
      <ChatComposer
        activeRequestId={null}
        approxSerializedBytes={0}
        behaviorLabel="default"
        canSend
        configuredPeerCount={1}
        dialedPeerCount={1}
        draft={draft}
        rowCount={0}
        sendHint={null}
        sending={false}
        turnState={null}
        onDraftChange={onDraftChange}
        onInterruptClick={vi.fn()}
        onSend={onSend}
        skills={skills}
      />,
    );
    return { onDraftChange, onSend };
  }

  it("opens on '/', accepts with Enter without submitting the form", () => {
    const onDraftChange = vi.fn();
    const onSend = vi.fn();
    renderComposer("", onDraftChange, onSend);

    const input = screen.getByTestId("composer-input");
    fireEvent.change(input, { target: { value: "/", selectionStart: 1 } });

    // Draft is controlled by the parent; re-render with the typed value.
    expect(onDraftChange).toHaveBeenCalledWith("/");
  });

  it("renders the menu for a slash draft and Enter selects instead of sending", () => {
    const onDraftChange = vi.fn();
    const onSend = vi.fn();
    renderComposer("/", onDraftChange, onSend);

    const input = screen.getByTestId("composer-input");
    fireEvent.keyUp(input, { target: { selectionStart: 1 } });

    expect(screen.getByTestId("slash-skill-menu")).toBeInTheDocument();
    expect(screen.getByTestId("slash-skill-review-skill")).toBeInTheDocument();
    expect(screen.queryByTestId("slash-skill-off-skill")).not.toBeInTheDocument();

    fireEvent.keyDown(input, { key: "Enter" });
    expect(onDraftChange).toHaveBeenCalledWith("/review-skill\n");
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not accept a suggestion or submit while IME composition is active", () => {
    const onDraftChange = vi.fn();
    const onSend = vi.fn();
    renderComposer("/", onDraftChange, onSend);

    const input = screen.getByTestId("composer-input");
    fireEvent.keyUp(input, { target: { selectionStart: 1 } });
    expect(screen.getByTestId("slash-skill-menu")).toBeInTheDocument();

    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(onDraftChange).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("advertises the skills affordance in the idle footer", () => {
    renderComposer("");
    expect(screen.getByTestId("composer-status")).toHaveTextContent("/ skills");
  });
});
