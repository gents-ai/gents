import { useEffect, useRef, useState } from "react";

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

/** Copies text that is still being read. WebKit refuses a clipboard write
    made after the click's task ends, so the write starts during the click
    with an item that waits for the text. */
export function copyLater(text: Promise<string>): Promise<void> {
  const clipboard = navigator.clipboard;
  if (!clipboard) return Promise.reject(new Error("the clipboard is unavailable"));
  if (typeof ClipboardItem !== "undefined" && clipboard.write) {
    const blob = text.then((t) => new Blob([t], { type: "text/plain" }));
    return clipboard.write([new ClipboardItem({ "text/plain": blob })]);
  }
  return text.then((t) => clipboard.writeText(t));
}
