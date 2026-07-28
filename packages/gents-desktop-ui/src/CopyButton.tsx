import { useEffect, useRef, useState } from "react";

import { copyText } from "./clipboard.js";

/// Small copy affordance for transcript content. `getText` is lazy so code
/// blocks can read their rendered textContent at click time.
export function CopyButton({
  getText,
  label = "Copy",
  className = "",
}: {
  getText: () => string;
  label?: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (timerRef.current != null) window.clearTimeout(timerRef.current);
    },
    [],
  );

  return (
    <button
      type="button"
      className={`copy-button ${className}`.trim()}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
      onClick={(event) => {
        event.stopPropagation();
        void copyText(getText()).then((ok) => {
          if (!ok) return;
          setCopied(true);
          if (timerRef.current != null) window.clearTimeout(timerRef.current);
          timerRef.current = window.setTimeout(() => setCopied(false), 1600);
        });
      }}
    >
      {copied ? "Copied" : label}
    </button>
  );
}
