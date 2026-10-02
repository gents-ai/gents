/* The pacing of streamed text, and the two seams where it used to vanish.
   The delivery shape is the real one: the runtime writes every
   stream_batch_ms (1,000) and the desktop picks it up on its own clock, so
   bursts of uneven size land about a second apart. */
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  closeOpenFence,
  createHandoff,
  holdLive,
  initialReveal,
  revealedText,
  stepReveal,
} from "@/screens/stream-reveal";

const FRAME = 16;

/* play `arrivals` (time → total length) through the reveal at 60fps and
   record what was on screen each frame */
function play(arrivals: [number, number][], until: number) {
  let state = initialReveal();
  let target = 0;
  let next = 0;
  const frames: { t: number; shown: number; target: number }[] = [];
  for (let t = 0; t <= until; t += FRAME) {
    while (next < arrivals.length && arrivals[next]![0] <= t)
      target = arrivals[next++]![1];
    state = stepReveal(state, target, t, FRAME);
    frames.push({ t, shown: state.shown, target });
  }
  return frames;
}

/* bursts as the desktop receives them: uneven sizes, 0.8–1.4s apart */
const bursts: [number, number][] = [
  [0, 40],
  [900, 260],
  [2_200, 300],
  [3_100, 620],
  [4_500, 700],
  [5_400, 1_050],
];

describe("revealing a batched stream", () => {
  const frames = play(bursts, 8_000);

  it("never goes backwards", () => {
    for (let i = 1; i < frames.length; i++)
      expect(frames[i]!.shown).toBeGreaterThanOrEqual(frames[i - 1]!.shown);
  });

  it("moves a few characters a frame, never a burst at once", () => {
    const steps = frames.slice(1).map((f, i) => f.shown - frames[i]!.shown);
    /* shown as it lands, the largest step is the largest burst: 350 */
    expect(Math.max(...steps)).toBeLessThanOrEqual(8);
  });

  it("does not stall between bursts while the stream is still coming", () => {
    /* from the first text to the last burst, count the longest run of
       frames where nothing moved though more was already there */
    let idle = 0;
    let longest = 0;
    for (const f of frames.filter((f) => f.t > 100 && f.t < 5_400)) {
      idle = f.shown === f.target ? idle + 1 : 0;
      longest = Math.max(longest, idle);
    }
    /* a burst drains a little slower than the next arrives */
    expect(longest * FRAME).toBeLessThanOrEqual(300);
  });

  it("trails the stream by about one burst, and catches up", () => {
    for (const f of frames) expect(f.target - f.shown).toBeLessThanOrEqual(420);
    expect(frames.at(-1)!.shown).toBe(1_050);
  });
});

describe("revealing a stream that arrives word by word", () => {
  const arrivals: [number, number][] = Array.from({ length: 80 }, (_, i) => [
    i * 45,
    (i + 1) * 6,
  ]);
  const frames = play(arrivals, 5_000);

  it("keeps up closely", () => {
    for (const f of frames) expect(f.target - f.shown).toBeLessThanOrEqual(24);
  });
});

describe("edges of the reveal", () => {
  it("shows replaced text rather than animating backwards", () => {
    let s = stepReveal(initialReveal(100, 100), 100, 0, FRAME);
    s = stepReveal(s, 30, 16, FRAME);
    expect(s.shown).toBe(30);
  });

  it("shows most of a huge burst at once rather than typing for seconds", () => {
    const s = stepReveal(initialReveal(), 5_000, 0, FRAME);
    expect(5_000 - s.shown).toBeLessThanOrEqual(1_200);
  });

  it("starts from what the live tail already showed", () => {
    const s = initialReveal(120, 400);
    expect(s.shown).toBe(120);
  });
});

describe("what is drawn", () => {
  it("ends on a word while more is coming", () => {
    expect(revealedText("make it **bold** now", 11)).toBe("make it");
    expect(revealedText("make it", 7)).toBe("make it");
  });

  it("does not split a surrogate pair", () => {
    const text = "ab😀";
    expect(revealedText(text, 3)).not.toMatch(/[\ud800-\udbff]$/);
  });

  it("closes a code block that has opened but not closed", () => {
    expect(closeOpenFence("see:\n```ts\nconst a = 1")).toBe(
      "see:\n```ts\nconst a = 1\n```",
    );
    expect(closeOpenFence("```ts\nx\n```")).toBe("```ts\nx\n```");
  });
});

describe("the handoff from live tail to message", () => {
  it("carries what was on screen to the message that continues it", () => {
    const h = createHandoff();
    h.noteShown("The export route reads ");
    expect(h.claim("m1", "The export route reads from the cache.")).toBe(22);
  });

  it("gives the same answer twice, for a render that runs twice", () => {
    const h = createHandoff();
    h.noteShown("Hello there");
    expect(h.claim("m1", "Hello there, friend")).toBe(11);
    expect(h.claim("m1", "Hello there, friend")).toBe(11);
  });

  it("does not claim a message that says something else", () => {
    const h = createHandoff();
    h.noteShown("Hello there");
    expect(h.claim("m2", "Something unrelated")).toBeNull();
  });
});

const live = (content: string | null): RenderedTimelineItem => ({
  kind: "liveAssistant",
  itemKey: "live-r1",
  content,
  reasoning: null,
});
const message = (content: string): RenderedTimelineItem => ({
  kind: "assistantMessage",
  itemKey: "a-r1",
  sequence: 2,
  content,
  reasoning: null,
  timestamp: null,
});
const person: RenderedTimelineItem = {
  kind: "userMessage",
  itemKey: "u1",
  sequence: 1,
  content: "Explain the export route.",
  timestamp: null,
};

describe("holding the live text through the gap", () => {
  it("records the live text while it streams", () => {
    const out = holdLive([person, live("The export route")], null);
    expect(out.held?.content).toBe("The export route");
  });

  it("puts it back under the same key when the tail is dropped first", () => {
    const held = holdLive([person, live("The export route")], null).held;
    const out = holdLive([person], held);
    expect(out.items.at(-1)).toMatchObject({
      kind: "liveAssistant",
      itemKey: "live-r1",
      content: "The export route",
    });
  });

  it("refills a tail whose text was cleared, keeping its key", () => {
    const held = holdLive([person, live("The export route")], null).held;
    const out = holdLive([person, live(null)], held);
    expect(out.items).toHaveLength(2);
    expect(out.items[1]).toMatchObject({
      itemKey: "live-r1",
      content: "The export route",
    });
  });

  it("is never shown in another session", () => {
    const held = holdLive([person, live("The export route")], null, "session-a").held;
    const out = holdLive([person], held, "session-b");
    expect(out.held).toBeNull();
    expect(out.items).toHaveLength(1);
  });

  it("is not let go by an earlier message that begins the same way", () => {
    const earlier = {
      ...message("The export route was slow yesterday."),
      itemKey: "a-earlier",
    };
    const held = holdLive([earlier, person, live("The export route")], null).held;
    const out = holdLive([earlier, person], held);
    expect(out.held).not.toBeNull();
    expect(out.items.at(-1)).toMatchObject({
      kind: "liveAssistant",
      content: "The export route",
    });
  });

  it("lets go once the message that continues it arrives", () => {
    const held = holdLive([person, live("The export route")], null).held;
    const out = holdLive(
      [person, message("The export route reads from the cache.")],
      held,
    );
    expect(out.held).toBeNull();
    expect(out.items).toHaveLength(2);
  });
});
