/* The bridge's pack commands return the JSON `gents pack` reports, which the
   bridge does not type (`serde_json::Value`); these are its shapes. */

/** A pack installed for the agent, and whether a newer version is published. */
export type InstalledPack = {
  pack: string;
  installed: string;
  latest: string | null;
  outdated: boolean;
};

/** A pack the registry search found. */
export type FoundPack = {
  namespace: string;
  name: string;
  description?: string | null;
  latest?: string | null;
  kind?: string;
};

/** An installed plugin that can call a model, and the profile it is bound to
    (none leaves it running without model calls). */
export type PackPluginSlot = {
  plugin: string;
  slot: string;
  profile: string | null;
};

/** A profile a plugin slot can be bound to. */
export type PackSlotProfile = {
  profile_id: string;
  display_name?: string | null;
  model_name: string;
  usable: boolean;
};

/** What an install or update does with documents someone edited: stop and
    name them, replace them, or keep the edits. */
export type PackEditedChoice = "refuse" | "overwrite" | "keep";

export type PackInstallRequest = {
  package: string;
  edited: PackEditedChoice;
  grantAuthority: boolean;
};
