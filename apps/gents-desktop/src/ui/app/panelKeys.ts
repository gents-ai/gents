import { chordAria, chordText, type Chord } from "@/commands/keys";
import { IN_DESKTOP, IS_MAC } from "@/screens/shortcutKeys";

/** The keys that go to the panel's first nine panels, as the picker numbers
    them: ⌃1–9 on a Mac, Alt+1–9 elsewhere (not Ctrl+Alt, which is AltGr on
    many keyboards). The desktop app's only: a browser has its own. */
export const NTH_PANEL = Array.from({ length: 9 }, (_, i) => ({
  mac: { key: String(i + 1), ctrl: true } as Chord,
  other: { key: String(i + 1), alt: true } as Chord,
}));

export const NTH_PANEL_TEXT = IS_MAC ? "⌃1–9" : "Alt+1–9";

/** The key for the panel numbered `n` (from 1), for its tooltip, where it
    works. */
export function nthPanelKey(n: number) {
  const keys = NTH_PANEL[n - 1];
  if (!keys || !IN_DESKTOP) return { text: undefined, aria: undefined };
  const chord = IS_MAC ? keys.mac : keys.other;
  return { text: chordText(chord), aria: chordAria(chord) };
}
