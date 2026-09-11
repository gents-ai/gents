import type { DeploymentView } from '@source-inc/gents-desktop-client'

/* two-letter initials on a pastel chip; the ink stays fixed like a marker */
export function initials(name: string) {
  const words = name.trim().split(/\s+/)
  return (words.length > 1 ? words[0]![0]! + words[1]![0]! : name.slice(0, 2)).replace(
    /^(.)(.)$/,
    (_, a: string, b: string) => a.toUpperCase() + b.toLowerCase(),
  )
}

export function behaviorName(behaviorId: string | null, deployment: DeploymentView | null) {
  return deployment?.behaviors.find((b) => b.behaviorId === behaviorId)?.displayName ?? 'Default'
}

/* A pastel per behaviour: one lightness and chroma, a hue spread around
   the wheel by the golden angle so neighbouring names never share a
   tint. Deep green ink (marker-foreground) reads on every hue. */
export function chipColor(name: string, hue?: number | null) {
  return `oklch(0.9 0.09 ${behaviorHue(name, hue).toFixed(1)})`
}
/* the same hue, deep enough to read as a ring on either ground */
export function ringColor(name: string, hue?: number | null) {
  return `oklch(0.72 0.14 ${behaviorHue(name, hue).toFixed(1)})`
}
function behaviorHue(name: string, hue?: number | null) {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return hue ?? (h * 137.508) % 360
}
/* the hues a person can pick instead, evenly around the wheel */
export const SWATCH_HUES = Array.from({ length: 12 }, (_, i) => i * 30)

/* The bridge's labels, as a person would say them. Files and bash come
   from the desktop's file_access_label / bash_access_label ("off",
   "read-only", "read / write", "unrestricted"); network is the tool
   selection's command_network_mode (disabled / inherit / enabled); the
   tool ceiling is init_tool_ceiling (meta-only / readonly / readwrite). */
export const fileAccess = (mode: string) =>
  ({ 'read / write': 'read and edit', 'read-only': 'read', off: 'not touch' })[mode] ?? mode
export const bashAccess = (mode: string) =>
  ({ unrestricted: 'run any', 'read-only': 'run read-only', off: 'not run' })[mode] ?? mode
/* the agent's tool ceiling, or a file label, as a phrase */
export const access = (mode: string) =>
  ({
    readwrite: 'read and edit files',
    readonly: 'read files',
    'meta-only': 'use meta tools only',
    'read / write': 'read and edit files',
    'read-only': 'read files',
    off: 'not touch files',
  })[mode] ?? mode
export const network = (mode: string | null | undefined) =>
  mode === 'enabled' ? 'the network' : mode === 'inherit' ? 'the network (inherited)' : 'no network'
