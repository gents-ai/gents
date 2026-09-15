/* How the side nav shows: the rail with a flyout on hover (default),
   always expanded as a column that pushes the canvas, or the rail alone.
   Stored next to the theme; a person's choice, not the app's. */
export type NavMode = "hover" | "expanded" | "collapsed";

const KEY = "gents-prototype-nav";

export function navPreference(): NavMode {
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "hover" || stored === "expanded" || stored === "collapsed")
      return stored;
  } catch {
    /* storage unavailable */
  }
  return "hover";
}

export function saveNavPreference(mode: NavMode) {
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    /* storage unavailable */
  }
}
