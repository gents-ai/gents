import { createContext, type ReactNode } from "react";
import type { ToolDiffLineView } from "@source-inc/gents-desktop-client";

export type TranscriptNavigation = {
  openFile(path: string, line?: number): void;
  openDiff(path: string, diff: ToolDiffLineView[]): void;
};

export const TranscriptNavigationContext =
  createContext<TranscriptNavigation | null>(null);

export function TranscriptNavigationProvider({
  children,
  navigation,
}: {
  children: ReactNode;
  navigation: TranscriptNavigation;
}) {
  return (
    <TranscriptNavigationContext.Provider value={navigation}>
      {children}
    </TranscriptNavigationContext.Provider>
  );
}
