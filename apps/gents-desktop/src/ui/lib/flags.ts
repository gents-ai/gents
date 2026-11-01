/**
 * Experimental features: presentation that is switched per device, from the
 * app's settings, before it ships to everyone.
 *
 * A flag gates presentation only. It never relaxes a permission, an access
 * check or a security boundary, and never changes what is written to a node:
 * an off flag and an on flag must both be safe for every person on every
 * device. Flags stay on this device and never sync to other devices or nodes.
 *
 * Each flag carries an owner and a date by which it is removed, either by
 * shipping what it gates or by deleting it; a test fails once the date
 * passes, so a stale flag forces that decision.
 */
export type Flag = {
  /** kebab-case, and stable: it is the key saved on the device */
  id: string;
  name: string;
  /** one plain sentence saying what turning it on does */
  description: string;
  default: boolean;
  owner: string;
  /** ISO date (YYYY-MM-DD) */
  removeBy: string;
  removeWhen: string;
  /** the gated code reads the flag once, at launch, so a change applies
      only after the app relaunches */
  restart: boolean;
};

export const FLAGS = [
  {
    id: "demo-feature",
    name: "Demo feature",
    description: "Turns on nothing; it shows how an experimental feature is switched.",
    default: false,
    owner: "david-on-github",
    removeBy: "2027-02-01",
    removeWhen:
      "The first real experimental feature is added and the tests use it instead.",
    restart: true,
  },
] as const satisfies readonly Flag[];

export type FlagId = (typeof FLAGS)[number]["id"];

/** what the person chose for a flag on this device; absent is its default */
export type FlagSetting = "on" | "off";
