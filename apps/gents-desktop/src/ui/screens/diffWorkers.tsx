import type { ReactNode } from "react";
import { DEFAULT_THEMES } from "@pierre/diffs";
import { WorkerPoolContextProvider } from "@pierre/diffs/react";
import WorkerUrl from "@pierre/diffs/worker/worker.js?worker&url";

/* All colouring runs in worker threads. On the page's own thread a long
   file held the view for seconds, and the first file in a language held it
   for about a second while the highlighter built that language's grammar,
   which stalled scrolling through a turn's files. A diff shows plain at
   once and takes its colours, fading in, when they are ready. Every diff
   shares one pool, which ends when the last diff goes. The workers run the
   same JavaScript engine the page would, since the app admits no
   WebAssembly. */
const POOL = {
  workerFactory: () => new Worker(WorkerUrl, { type: "module" }),
  poolSize: 2,
};
const HIGHLIGHTING = {
  theme: { light: DEFAULT_THEMES.light, dark: DEFAULT_THEMES.dark },
  langs: [],
  preferredHighlighter: "shiki-js" as const,
};

export function DiffWorkers({ children }: { children: ReactNode }) {
  return (
    <WorkerPoolContextProvider poolOptions={POOL} highlighterOptions={HIGHLIGHTING}>
      {children}
    </WorkerPoolContextProvider>
  );
}
