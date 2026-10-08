/* This browser's storage, for state that outlives the visit. Storage can be
   unavailable or refuse a write; then that state lasts the run. */
export const browserStorage = {
  getItem(key: string) {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  setItem(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* storage unavailable */
    }
  },
  removeItem(key: string) {
    try {
      localStorage.removeItem(key);
    } catch {
      /* storage unavailable */
    }
  },
};

const isStrings = (v: unknown): v is unknown[] => Array.isArray(v);

/** The strings in what storage held: storage is outside the app's control
    (another build, a hand edit), so anything else is left out. */
export const storedStrings = (v: unknown): string[] =>
  isStrings(v) ? v.filter((x): x is string => typeof x === "string") : [];
