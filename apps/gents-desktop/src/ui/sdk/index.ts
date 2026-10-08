/* The one importable surface for a UX plugin: `@gents/ux-sdk`. Bundled
   plugins resolve it through the Vite alias; runtime-loaded plugins get the
   same object through the blob shims in ./runtime.ts. Everything a plugin
   needs lives here, so the import allowlist can stay at three specifiers.
   The list is additive: a name exported once is never removed. After
   hermes-agent's sdk/index.ts, cut to what this app has. */

/* contract */
export type {
  CreateContextOptions,
  GentsUxPlugin,
  UxContext,
  UxContribution,
  UxHost,
  UxOs,
  UxShell,
  UxStorage,
} from "@/contrib/plugin";
export { UndeclaredContributionError } from "@/contrib/plugin";

/* areas and payloads */
export {
  AGENT_SECTIONS_AREA,
  KNOWN_AREAS,
  NAV_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@/contrib/types";
export type {
  AgentSectionData,
  AgentSectionProps,
  Contribution,
  ContributionSource,
  KnownArea,
  TranscriptDirectiveContribution,
  TranscriptDirectiveProps,
} from "@/contrib/types";
export type { NavContext, NavItem } from "@/app/navRegistry";
export type { UxEvent, UxEventListener } from "@/contrib/events";
export type { Route } from "@/lib/router";

/* react helpers */
export { ContribBoundary } from "@/contrib/react/boundary";
export { Slot } from "@/contrib/react/slot";
export { useContributions } from "@/contrib/react/use-contributions";
export { useUxPluginRecords } from "@/contrib/plugins-store";
export type { UxPluginRecord } from "@/contrib/plugins-store";

/* directive parsing, for a plugin that renders prose of its own */
export { parseTranscriptDirective } from "@/contrib/directives";

/* the app's own primitives, so a plugin's UI reads like the app's */
export { href } from "@/lib/router";
export { Markdown } from "@/screens/Markdown";
export { Hint } from "@/screens/Hint";
export { Fact, Group, Row, StackedRow } from "@/screens/agent/rows";
export { Button } from "@gents/ui/components/button";
export { Badge } from "@gents/ui/components/badge";
export { Input } from "@gents/ui/components/input";
export { Textarea } from "@gents/ui/components/textarea";
export { Switch } from "@gents/ui/components/switch";
export { Separator } from "@gents/ui/components/separator";
export { ScrollArea } from "@gents/ui/components/scroll-area";
export { Skeleton } from "@gents/ui/components/skeleton";
export {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@gents/ui/components/card";
export {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@gents/ui/components/dialog";
export { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
export {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";
export { cn } from "@gents/ui/lib/utils";
export { toast } from "sonner";
