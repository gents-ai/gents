/* a moment, as a person would say it: the age alone, since every place
   that shows one is a list where "ago" would repeat on every row */
export const when = (iso: string | null) => {
  if (!iso) return "";
  const mins = Math.round((Date.now() - Date.parse(iso)) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  if (mins < 1_440) return `${Math.round(mins / 60)}h`;
  if (mins < 7 * 1_440) return `${Math.round(mins / 1_440)}d`;
  return new Date(iso).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
};

/* a length of time, as a person would say it: never below a minute */
export const span = (ms: number) => {
  const mins = Math.max(1, Math.round(ms / 60_000));
  if (mins < 60) return `${mins}m`;
  if (mins < 1_440) return `${Math.round(mins / 60)}h`;
  return `${Math.round(mins / 1_440)}d`;
};
