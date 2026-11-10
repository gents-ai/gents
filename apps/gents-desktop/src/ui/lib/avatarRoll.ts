import { useLayoutEffect, useState } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

import { AVATARS, avatarSrc } from "./avatar";

type Roll = { from: string; to: string; turn: number };

type RollState = {
  /* the avatar each node shows until its saved one catches up */
  chosen: Record<string, string>;
  rolls: Record<string, Roll>;
};

const store = createStore<RollState>(() => ({ chosen: {}, rolls: {} }));
let turns = 0;

/* portraits that pass the window before the new one lands */
const PASSING = 10;
export const SPIN_MS = 650;

/** Rolls every avatar showing `id` from `from` to `to`, all in step, and has
    them show `to` until `settleAvatar`. */
export function rollAvatar(id: string, from: string, to: string) {
  turns += 1;
  const roll = { from, to, turn: turns };
  store.setState((s) => ({
    chosen: { ...s.chosen, [id]: to },
    rolls: { ...s.rolls, [id]: roll },
  }));
}

/** Shows `key` for `id` without a roll. */
export function holdAvatar(id: string, key: string) {
  store.setState((s) => ({ ...s, chosen: { ...s.chosen, [id]: key } }));
}

/** Returns `id` to the avatar its owner gives it. */
export function settleAvatar(id: string) {
  store.setState((s) => {
    if (!(id in s.chosen)) return s;
    const chosen = { ...s.chosen };
    delete chosen[id];
    return { ...s, chosen };
  });
}

/** The avatar a roll shows for `id` while its save is under way. */
export const chosenAvatar = (id: string): string | undefined =>
  store.getState().chosen[id];

export const useChosenAvatar = (id: string | undefined) =>
  useStore(store, (s) => (id ? s.chosen[id] : undefined));

/* seeded by the turn, so every avatar rolling it passes the same portraits */
function reelOf({ from, to, turn }: Roll) {
  let seed = (turn * 2654435761) >>> 0;
  const next = () => {
    seed = (seed + 0x6d2b79f5) >>> 0;
    let t = seed;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) % AVATARS;
  };
  const reel = [from];
  while (reel.length <= PASSING) {
    const key = String(next() + 1);
    if (key !== reel[reel.length - 1] && key !== to) reel.push(key);
  }
  return [...reel, to];
}

/* vertical blurs for a reel in motion, strongest first; in units of the
   reel's height, which is always PASSING + 2 portraits */
const BLURS = [0.0025, 0.005, 0.008];
function addReelBlurs() {
  if (document.getElementById("avatar-reel-blurs")) return;
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.id = "avatar-reel-blurs";
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("width", "0");
  svg.setAttribute("height", "0");
  svg.style.position = "absolute";
  const defs = document.createElementNS(ns, "defs");
  BLURS.forEach((deviation, i) => {
    const filter = document.createElementNS(ns, "filter");
    filter.id = `avatar-reel-blur-${i + 1}`;
    filter.setAttribute("primitiveUnits", "objectBoundingBox");
    const blur = document.createElementNS(ns, "feGaussianBlur");
    blur.setAttribute("stdDeviation", `0 ${deviation}`);
    filter.append(blur);
    defs.append(filter);
  });
  svg.append(defs);
  document.body.append(svg);
}

/* portraits known decoded, so a roll of them starts on its click's frame */
const decodedSrcs = new Set<string>();
/* one decode per reel, so every avatar rolling it starts on the same frame */
const decoding = new Map<number, Promise<unknown>>();
function decode(roll: Roll, reel: string[]): Promise<unknown> | null {
  addReelBlurs();
  const missing = reel.map(avatarSrc).filter((src) => !decodedSrcs.has(src));
  if (!missing.length) return null;
  let ready = decoding.get(roll.turn);
  if (!ready) {
    ready = Promise.all(
      missing.map((src) => {
        const img = new Image();
        img.src = src;
        return img
          .decode()
          .then(() => decodedSrcs.add(src))
          .catch(() => undefined);
      }),
    );
    decoding.set(roll.turn, ready);
  }
  return ready;
}

let preloaded = false;
/** Decodes every portrait ahead of a roll, once. */
export function preloadAvatars() {
  if (preloaded) return;
  preloaded = true;
  for (let n = 1; n <= AVATARS; n++) {
    const src = avatarSrc(String(n));
    if (decodedSrcs.has(src)) continue;
    const img = new Image();
    img.src = src;
    void img
      .decode()
      .then(() => decodedSrcs.add(src))
      .catch(() => undefined);
  }
}

const reducedMotion = () =>
  typeof matchMedia === "function" &&
  matchMedia("(prefers-reduced-motion: reduce)").matches;

export type Reel = { run: string[]; turn: number; landed?: boolean };

/** The reel an avatar showing `id` runs when it rolls, null when it has not.
    A roll from before the avatar mounted is not replayed. */
export function useAvatarReel(id: string | undefined) {
  const roll = useStore(store, (s) => (id ? s.rolls[id] : undefined));
  const [seen] = useState(roll?.turn);
  const [reel, setReel] = useState<Reel | null>(null);
  /* before paint, so the new portrait never shows ahead of the reel */
  useLayoutEffect(() => {
    if (!roll || roll.turn === seen || reducedMotion()) return;
    const run = reelOf(roll);
    const ready = decode(roll, run);
    if (!ready) {
      setReel({ run, turn: roll.turn });
      return;
    }
    let live = true;
    setReel({ run: [roll.from], turn: -roll.turn });
    void ready.then(() => {
      if (live) setReel({ run, turn: roll.turn });
    });
    return () => {
      live = false;
    };
  }, [roll, seen]);
  /* a landed reel stays in place, so the next roll starts from it */
  return { reel, land: () => setReel((r) => r && { ...r, landed: true }) };
}

/* the reel streaks while it runs fast and lands sharp */
const STREAKS: Keyframe[] = [
  { offset: 0, filter: "url(#avatar-reel-blur-3)" },
  { offset: 0.25, filter: "url(#avatar-reel-blur-2)" },
  { offset: 0.45, filter: "url(#avatar-reel-blur-1)" },
  { offset: 0.6, filter: "none" },
  { offset: 1, filter: "none" },
];

/** Runs `strip`, a column of `length` portraits, up to its last, calling
    `done` when it lands; returns a stop. */
export function spinReel(strip: HTMLElement, length: number, done: () => void) {
  if (length < 2 || typeof strip.animate !== "function") return () => {};
  const travel = strip.animate(
    [
      { transform: "translateY(0)" },
      { transform: `translateY(-${((length - 1) / length) * 100}%)` },
    ],
    { duration: SPIN_MS, easing: "cubic-bezier(0.12, 0.8, 0.2, 1)", fill: "forwards" },
  );
  const streak = strip.animate(STREAKS, { duration: SPIN_MS });
  travel.onfinish = done;
  return () => {
    travel.onfinish = null;
    travel.cancel();
    streak.cancel();
  };
}
