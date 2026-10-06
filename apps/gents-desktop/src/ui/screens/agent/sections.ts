/* The agent configuration screen's sections: an extension point. Each is a
   route (`agent/<id>`), so a section is linkable, and a section in the
   sidebar names its group, label and mark. A route that has no entry of its
   own (an older link, or a page reached from another section) names the
   entry it lights up instead. */
import type { ComponentType } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { createRegistry } from "@/app/registry";

export type SectionPanelProps = {
  deployment: DeploymentView;
  /** the document the route opens, when it names one */
  item?: string;
};

type Listed = {
  /** the sidebar's group; groups show in the order their first section registered */
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  /** how many the sidebar shows beside the label */
  count?: (deployment: DeploymentView) => number;
};

export type AgentSection = {
  id: string;
  Panel: ComponentType<SectionPanelProps>;
} & (Listed | { under: string });

export const agentSections = createRegistry<AgentSection>();

export const isListed = (section: AgentSection): section is AgentSection & Listed =>
  !("under" in section);
