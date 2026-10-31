import { describe, expect, it } from "vitest";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { recalledParts, sentMessages } from "../src/ui/screens/session/composerHistory";

const user = (
  requestId: string,
  content: string | null,
  selectedSkillIds: string[] = [],
): RenderedTimelineItem =>
  ({
    kind: "userMessage",
    itemKey: `user:${requestId}`,
    requestId,
    ownsTurn: true,
    sequence: null,
    content,
    selectedSkillIds,
    timestamp: null,
  }) as unknown as RenderedTimelineItem;

const pending = (requestId: string, content: string): RenderedTimelineItem =>
  ({
    kind: "pendingUserTurn",
    itemKey: `pending:${requestId}`,
    requestId,
    content,
    selectedSkillIds: [],
    lifecycleState: null,
    foldedIntoRequestId: null,
    origin: null,
    createdAt: null,
  }) as unknown as RenderedTimelineItem;

const chip = (id: string) =>
  ({ type: "chip", chip: { kind: "command", id, label: `/${id}` } }) as const;
const text = (value: string) => ({ type: "text", text: value }) as const;

describe("the messages ↑ steps back through", () => {
  it("are the person's own, oldest first, with their skills", () => {
    const automated = {
      kind: "automatedInput",
      itemKey: "goal",
      content: "Continue the goal",
    } as unknown as RenderedTimelineItem;
    const reply = {
      kind: "assistantMessage",
      itemKey: "reply",
      content: "Done",
    } as unknown as RenderedTimelineItem;
    expect(
      sentMessages([
        user("a", "fix the build"),
        reply,
        automated,
        user("b", "please /review it", ["review"]),
      ]),
    ).toEqual([
      { text: "fix the build", skills: [] },
      { text: "please /review it", skills: ["review"] },
    ]);
  });

  it("count a turn once while it is both pending and in the transcript", () => {
    expect(sentMessages([user("a", "first"), pending("a", "first")])).toEqual([
      { text: "first", skills: [] },
    ]);
  });

  it("take skill lines off a message gents has not read yet", () => {
    expect(sentMessages([pending("a", "/skill review\nlook at this")])).toEqual([
      { text: "look at this", skills: ["review"] },
    ]);
  });

  it("step over the same message sent again in a row, and over empty ones", () => {
    expect(
      sentMessages([
        user("a", "again"),
        user("b", "again"),
        user("c", "  "),
        user("d", "", ["review"]),
        user("e", "again"),
      ]),
    ).toEqual([
      { text: "again", skills: [] },
      { text: "", skills: ["review"] },
      { text: "again", skills: [] },
    ]);
  });
});

describe("a message brought back to the composer", () => {
  it("is its text when it ran no skills", () => {
    expect(recalledParts({ text: "fix /etc/hosts", skills: [] })).toEqual([
      text("fix /etc/hosts"),
    ]);
  });

  it("has each skill's chip where its text names it", () => {
    expect(recalledParts({ text: "please /review now", skills: ["review"] })).toEqual([
      text("please "),
      chip("review"),
      text("now"),
    ]);
  });

  it("puts a skill its text doesn't name in front, as a chip", () => {
    expect(
      recalledParts({ text: "check /lint output", skills: ["review", "lint"] }),
    ).toEqual([chip("review"), text("check "), chip("lint"), text("output")]);
    expect(recalledParts({ text: "", skills: ["review"] })).toEqual([chip("review")]);
  });

  it("names a skill once though the text repeats it", () => {
    expect(
      recalledParts({ text: "/review then /review again", skills: ["review"] }),
    ).toEqual([chip("review"), text("then /review again")]);
  });

  it("keeps its lines", () => {
    expect(
      recalledParts({ text: "/review\nfirst line\nsecond", skills: ["review"] }),
    ).toEqual([chip("review"), text("\nfirst line\nsecond")]);
  });
});
