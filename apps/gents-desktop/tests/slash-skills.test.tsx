import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer } from "@source-inc/gents-desktop-chat";
import {
  applySkillSelection,
  effectiveContextSkills,
  slashSkillSuggestion,
} from "@source-inc/gents-desktop-chat";
import type { AgentContext, SkillView } from "@source-inc/gents-desktop-client";

const skills: SkillView[] = [
  {
    skillId: "review-skill",
    agentDid: "did:key:z6MkAgent",
    name: "Review",
    description: null,
    instructions: null,
    toolRefs: [],
    displayName: null,
    enabled: true,
    createdAt: null,
  },
  {
    skillId: "deploy-skill",
    agentDid: "did:key:z6MkAgent",
    name: "Deploy",
    description: null,
    instructions: null,
    toolRefs: [],
    displayName: null,
    enabled: true,
    createdAt: null,
  },
  {
    skillId: "off-skill",
    agentDid: "did:key:z6MkAgent",
    name: "Disabled",
    description: null,
    instructions: null,
    toolRefs: [],
    displayName: null,
    enabled: false,
    createdAt: null,
  },
];

function context(skillIds: string[] | null): AgentContext {
  return {
    context_id: "ctx-default",
    agent_did: "did:key:z6MkAgent",
    system_prompt: null,
    tools_id: null,
    compaction_id: null,
    skill_ids: skillIds,
    tags: null,
  };
}

describe("effectiveContextSkills", () => {
  it("applies the active context's skill_ids as an explicit whitelist", () => {
    const deploymentSkills: SkillView[] = [
      { ...skills[0], skillId: "context-allowed" },
      { ...skills[0], skillId: "context-unlisted" },
      { ...skills[2], skillId: "context-disabled-allowed" },
    ];

    expect(
      effectiveContextSkills(
        deploymentSkills,
        context(["context-allowed", "context-disabled-allowed"]),
      ).map((skill) => skill.skillId),
    ).toEqual(["context-allowed"]);
  });

  it("resolves no skills without an active context", () => {
    expect(effectiveContextSkills(skills, null)).toEqual([]);
    expect(effectiveContextSkills(skills, undefined)).toEqual([]);
  });

  it("allows no skills when the whitelist is empty or absent", () => {
    expect(effectiveContextSkills(skills, context([]))).toEqual([]);
    expect(effectiveContextSkills(skills, context(null))).toEqual([]);
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
        activityStatus={null}
        approxSerializedBytes={0}
        behaviorLabel="default"
        canSend
        draft={draft}
        interruptVisible={false}
        rowCount={0}
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
