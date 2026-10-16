export type InstalledPack = {
  pack: string;
  installed: string;
  latest: string | null;
  outdated: boolean;
};

export type FoundPack = {
  namespace: string;
  name: string;
  description?: string | null;
  latest?: string | null;
  kind?: string;
};

export type PackPluginSlot = {
  plugin: string;
  slot: string;
  profile: string | null;
};

export type PackSlotProfile = {
  profile_id: string;
  display_name?: string | null;
  model_name: string;
  usable: boolean;
};

export type PackEditedChoice = "refuse" | "overwrite" | "keep";

export type PackInstallRequest = {
  package: string;
  edited: PackEditedChoice;
  grantAuthority: boolean;
};
