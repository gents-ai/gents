/* a moment, as a person would say it */
export const when = (iso: string | null) => {
  if (!iso) return "";
  const mins = Math.round((Date.now() - Date.parse(iso)) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  if (mins < 1_440) return `${Math.round(mins / 60)}h ago`;
  if (mins < 7 * 1_440) return `${Math.round(mins / 1_440)} days ago`;
  return new Date(iso).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
};
