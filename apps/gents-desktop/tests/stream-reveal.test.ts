/* The pacing of streamed text, and the two seams where it used to vanish.
   The delivery shape is the real one: the runtime writes every
   stream_batch_ms (1,000) and the desktop picks it up on its own clock, so
   bursts of uneven size land about a second apart. */
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  closeOpenFence,
  drawKey,
  drawKeys,
  holdLive,
  initialReveal,
  noDrawKeys,
  revealedText,
  stepReveal,
} from "@/screens/stream-reveal";
import { assistantMessage, liveAssistant, userMessage } from "./timeline-fixture";

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

const live = (content: string | null): RenderedTimelineItem =>
  liveAssistant({
    kind: "liveAssistant",
    itemKey: "live-r1",
    content,
    reasoning: null,
  });
const message = (content: string): RenderedTimelineItem =>
  assistantMessage({
    kind: "assistantMessage",
    itemKey: "a-r1",
    sequence: 2,
    content,
    reasoning: null,
    timestamp: null,
  });
const person: RenderedTimelineItem = userMessage({
  kind: "userMessage",
  itemKey: "u1",
  sequence: 1,
  content: "Explain the export route.",
  timestamp: null,
});

describe("holding the live text through the gap", () => {
  it("records the live text while it streams", () => {
    const out = holdLive([person, live("The export route")], null);
    expect(out.held?.content).toBe("The export route");
  });

  it("puts it back under the same key when the tail is dropped first", () => {
    const held = holdLive([person, live("The export route")], null).held;
    const out = holdLive([person], held);
    expect(out.items.at(-1)).toMatchObject(
      liveAssistant({
        kind: "liveAssistant",
        itemKey: "live-r1",
        content: "The export route",
      }),
    );
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
    expect(out.replacedBy).toBe("a-r1");
    expect(out.items).toHaveLength(2);
  });
});

describe("the keys replies are drawn under", () => {
  it("draws the message that replaces a live tail under the tail's key", () => {
    const streaming = drawKeys(
      noDrawKeys("s"),
      [person, live("The export")],
      undefined,
      "s",
    );
    const tailKey = drawKey(streaming, live("The export"));
    const saved = message("The export route reads from the cache.");
    const replaced = drawKeys(streaming, [person, saved], "a-r1", "s");
    expect(drawKey(replaced, saved)).toBe(tailKey);
  });

  it("gives the next turn's tail a key of its own", () => {
    const first = drawKeys(noDrawKeys("s"), [live("One")], undefined, "s");
    const saved = drawKeys(first, [message("One")], "a-r1", "s");
    const next = drawKeys(saved, [message("One"), live("Two")], undefined, "s");
    expect(drawKey(next, live("Two"))).not.toBe(drawKey(next, message("One")));
  });

  it("gives the same keys to a render that runs twice", () => {
    const streaming = drawKeys(noDrawKeys("s"), [live("One")], undefined, "s");
    const once = drawKeys(streaming, [message("One")], "a-r1", "s");
    const twice = drawKeys(once, [message("One")], "a-r1", "s");
    expect(drawKey(twice, message("One"))).toBe(drawKey(once, message("One")));
  });

  it("starts again for another session", () => {
    const streaming = drawKeys(noDrawKeys("a"), [live("One")], undefined, "a");
    const saved = drawKeys(streaming, [message("One")], "a-r1", "a");
    const other = drawKeys(saved, [message("One")], undefined, "b");
    expect(drawKey(other, message("One"))).toBe("a-r1");
  });
});
