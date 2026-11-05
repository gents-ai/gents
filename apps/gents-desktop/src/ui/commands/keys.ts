import { IS_MAC } from "@/screens/shortcutKeys";

/** A key and the modifiers held with it. `mod` is the app's command key: ⌘
    on a Mac, Ctrl elsewhere. `ctrl` is the Control key itself, which on a
    Mac is ⌃ and elsewhere is the same key as `mod`. `key` is the key's value
    (`KeyboardEvent.key`), lowercased for letters. */
export type Chord = {
  key: string;
  mod?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
};

/** A command's keys: one chord on every platform, or one each. */
export type KeySpec = Chord | { mac?: Chord; other?: Chord };

const isChord = (spec: KeySpec): spec is Chord => "key" in spec;

/** the chord a spec gives this platform, if any */
export const chordOn = (spec: KeySpec | undefined, mac = IS_MAC): Chord | null =>
  !spec ? null : isChord(spec) ? spec : ((mac ? spec.mac : spec.other) ?? null);

/* the physical modifiers a chord asks for */
const held = (chord: Chord, mac: boolean) => ({
  meta: mac && !!chord.mod,
  ctrl: mac ? !!chord.ctrl : !!(chord.mod || chord.ctrl),
  alt: !!chord.alt,
  shift: !!chord.shift,
});

/** Whether a key press is exactly this chord: the same key and the same
    modifiers, no more, so ⌃⌘J is not ⌘J and ⌃K stays with text editing. */
export function matches(
  chord: Chord,
  e: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">,
  mac = IS_MAC,
): boolean {
  const want = held(chord, mac);
  return (
    e.metaKey === want.meta &&
    e.ctrlKey === want.ctrl &&
    e.altKey === want.alt &&
    e.shiftKey === want.shift &&
    e.key.toLowerCase() === chord.key.toLowerCase()
  );
}

const SHOWN: Record<string, string> = {
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  enter: "↵",
  escape: "Esc",
};
const shownKey = (key: string) => SHOWN[key.toLowerCase()] ?? key.toUpperCase();

/** How a chord is written: "⌃⌥⇧⌘K" on a Mac, "Ctrl+Alt+Shift+K" elsewhere. */
export function chordText(chord: Chord, mac = IS_MAC): string {
  const h = held(chord, mac);
  if (mac)
    return `${h.ctrl ? "⌃" : ""}${h.alt ? "⌥" : ""}${h.shift ? "⇧" : ""}${h.meta ? "⌘" : ""}${shownKey(chord.key)}`;
  return [h.ctrl && "Ctrl", h.alt && "Alt", h.shift && "Shift", shownKey(chord.key)]
    .filter(Boolean)
    .join("+");
}

/** The same for `aria-keyshortcuts`, e.g. "Meta+Shift+J". */
export function chordAria(chord: Chord, mac = IS_MAC): string {
  const h = held(chord, mac);
  const key = chord.key.length === 1 ? chord.key.toUpperCase() : chord.key;
  return [
    h.ctrl && "Control",
    h.alt && "Alt",
    h.shift && "Shift",
    h.meta && "Meta",
    key,
  ]
    .filter(Boolean)
    .join("+");
}

/* Keys a person has changed on this device, by command id; null takes a
   command's key away. Nothing sets these yet: it is where an editor for
   keys in Settings will write. */
let overrides: Readonly<Record<string, Chord | null>> = {};
export const setKeyOverrides = (next: Readonly<Record<string, Chord | null>>) => {
  overrides = next;
};

/** The chord a command answers to here: a person's own, else its default. */
export const effectiveChord = (id: string, spec: KeySpec | undefined, mac = IS_MAC) =>
  id in overrides ? overrides[id]! : chordOn(spec, mac);
