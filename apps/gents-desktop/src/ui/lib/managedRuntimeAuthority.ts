import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";

export function authorityForSelection(
  toolCeiling: ManagedServerAuthorityInput["toolCeiling"],
  toolRoot: string | null,
): ManagedServerAuthorityInput | null {
  if (toolCeiling === "meta-only") return { toolCeiling, toolRoot: null };
  return toolRoot ? { toolCeiling, toolRoot } : null;
}

export function authoritySummary(authority: ManagedServerAuthorityInput): string {
  switch (authority.toolCeiling) {
    case "readwrite":
      return "The managed runtime can read and modify files under this root and run unrestricted commands as your user. Commands are not a filesystem sandbox and may reach other locations your OS account can access; operating-system privacy controls still apply.";
    case "readonly":
      return "The managed runtime can read files under this root and run its restricted read-only command set. It cannot modify files through host tools.";
    case "meta-only":
      return "The managed runtime receives no host file or shell authority. Configuration and remote services remain available where separately configured.";
  }
}

export function authoritiesEqual(
  left: ManagedServerAuthorityInput,
  right: ManagedServerAuthorityInput,
): boolean {
  return left.toolCeiling === right.toolCeiling && left.toolRoot === right.toolRoot;
}
