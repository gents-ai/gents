// This browser's storage, for state that outlives the visit; storage can be
// unavailable or refuse a write, and then that state lasts the run.
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
    }
  },
  removeItem(key: string) {
    try {
      localStorage.removeItem(key);
    } catch {
    }
  },
};

const isStrings = (v: unknown): v is unknown[] => Array.isArray(v);

export const storedStrings = (v: unknown): string[] =>
  isStrings(v) ? v.filter((x): x is string => typeof x === "string") : [];
