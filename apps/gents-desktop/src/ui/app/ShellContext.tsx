/* The shell, for surfaces. A surface is mounted by the dock, which the
   app shell draws without knowing the shell; this hands it down without
   threading it through every layout prop. */
import { createContext, useContext } from "react";
import type { Shell } from "@/hooks/useShell";

const ShellContext = createContext<Shell | null>(null);
export const ShellProvider = ShellContext.Provider;

export function useShellContext(): Shell {
  const shell = useContext(ShellContext);
  if (!shell) throw new Error("useShellContext: no ShellProvider above");
  return shell;
}
