import type { ComponentType } from "react";
import { FlaskConical, SlidersHorizontal, Wrench } from "lucide-react";
import { DeveloperPanel } from "./DeveloperPanel";
import { ExperimentalPanel } from "./ExperimentalPanel";
import { GeneralPanel } from "./GeneralPanel";

export type SettingsSection = {
  id: string;
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  Panel: ComponentType;
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
