/** A span as the compact text a timer reads: "<1s", "42s", "4m 12s",
    "5m", "1h 3m", "2h", "1d 4h". It rounds down, so it never says more
    time passed than did, and gives at most its two largest units. */
export function duration(ms: number): string {
  const secs = Math.floor(ms / 1000);
  if (secs < 1) return "<1s";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return secs % 60 ? `${mins}m ${secs % 60}s` : `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return mins % 60 ? `${hours}h ${mins % 60}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days}d ${hours % 24}h` : `${days}d`;
}
