import { useEffect, useRef, useState } from "react";

/** Copies text and says so for a moment. The clipboard may refuse; only a
    copy that happened is claimed, and a second copy restarts the moment. */
export function useCopied(ms = 1200) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);
  const copy = (text: string) => {
    navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopied(true);
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(() => setCopied(false), ms);
      })
      .catch(() => {});
  };
  return { copied, copy };
}
