/* State that outlives the visit: a list of strings kept in local storage
   under a key, so a narrowing a person set is there when they come back.
   The default is what an absent or unreadable value means. */
import { useEffect, useState } from "react";

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

export function useStoredStrings(key: string, initial: readonly string[] = []) {
  const [value, setValue] = useState<string[]>(() => {
    try {
      const raw = localStorage.getItem(key);
      return raw ? strings(JSON.parse(raw)) : [...initial];
    } catch {
      return [...initial];
    }
  });
  /* always written: the default can be computed before the data that
     shapes it arrives, and dropping the key on a passing match lost picks */
  useEffect(() => {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      /* storage unavailable */
    }
  }, [key, value]);
  return [value, setValue] as const;
}
