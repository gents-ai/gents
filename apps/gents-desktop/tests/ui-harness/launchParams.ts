import { FLAGS, type FlagId } from "../../src/ui/lib/flags";
import { preferences, resetPreferences } from "../../src/ui/preferences";

/* Imported before the app, whose modules read the experimental features once
   as they load. ?fresh forgets what this browser remembers of earlier visits,
   so what opens once opens again, and launches with the flags ?flags=a,b
   names; without ?fresh those flags are turned on over what is remembered. */
const params = new URLSearchParams(window.location.search);
const on = (params.get("flags") ?? "")
  .split(",")
  .filter((id): id is FlagId => FLAGS.some((flag) => flag.id === id));
if (params.has("fresh"))
  resetPreferences(Object.fromEntries(on.map((id) => [id, "on"] as const)));
else for (const id of on) preferences.setFlag(id, "on");
