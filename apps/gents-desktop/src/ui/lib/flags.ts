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
    restart: false,
  },
  {
    id: "automations",
    name: "Automations",
    description:
      "An Automations page for the active node: what runs on its own, when it runs next, and how its runs went.",
    default: false,
    owner: "david-on-github",
    removeBy: "2027-03-01",
    removeWhen: "The Automations page ships to everyone.",
    restart: false,
  },
  {
    id: "notifications",
    name: "Notifications",
    description:
      "Tells you through the system's notifications when a session finishes, fails or needs your answer, or the mailbox gets something new, while the app's window is in the background.",
    default: false,
    owner: "david-on-github",
    removeBy: "2027-02-01",
    removeWhen:
      "Notifications ship to everyone, fed by the app's one place for things that need the person.",
    restart: false,
  },
  {
    id: "pack-notifications",
    name: "Notifications from packs",
    description:
      "Lets a pack's page tell you through the system's notifications, for each kind you turn on under Notifications. Needs Notifications on too.",
    default: false,
    owner: "david-on-github",
    removeBy: "2027-02-01",
    removeWhen:
      "What a pack's notification may carry outside the app is decided, and they ship to everyone or are dropped.",
    restart: false,
  },
] as const satisfies readonly Flag[];

export type FlagId = (typeof FLAGS)[number]["id"];

/** what the person chose for a flag on this device; absent is its default */
export type FlagSetting = "on" | "off";
