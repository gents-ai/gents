import { useMemo, type ComponentType } from "react";
import { Bell, FlaskConical, SlidersHorizontal, Wrench } from "lucide-react";
import type { FlagId } from "@/lib/flags";
import { flag, usePreferences } from "@/preferences";
import { DeveloperPanel } from "./DeveloperPanel";
import { ExperimentalPanel } from "./ExperimentalPanel";
import { GeneralPanel } from "./GeneralPanel";
import { NotificationsGroup } from "./NotificationsGroup";

export type SettingsSection = {
  id: string;
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  Panel: ComponentType;
  /** shown only while this experimental feature is on */
  flag?: FlagId;
};

/* the app's own settings, as the person sees them: what applies on this
   computer, not a node's configuration, which stays on its Node page */
export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    group: "App",
    id: "general",
    label: "General",
    icon: SlidersHorizontal,
    Panel: GeneralPanel,
  },
  {
    group: "App",
    id: "notifications",
    label: "Notifications",
    icon: Bell,
    Panel: NotificationsGroup,
    flag: "notifications",
  },
  {
    group: "App",
    id: "developer",
    label: "Developer",
    icon: Wrench,
    Panel: DeveloperPanel,
  },
  {
    group: "App",
    id: "experimental",
    label: "Experimental features",
    icon: FlaskConical,
    Panel: ExperimentalPanel,
  },
];

export const DEFAULT_SETTINGS_SECTION = SETTINGS_SECTIONS[0]!.id;

/** The sections shown now: those behind an experimental feature only while
    it is on. */
export function useSettingsSections() {
  const flags = usePreferences((p) => p.flags);
  return useMemo(() => shownSettingsSections(), [flags]);
}

/** `useSettingsSections` outside React, as of now. */
export const shownSettingsSections = () =>
  SETTINGS_SECTIONS.filter((s) => !s.flag || flag(s.flag));
