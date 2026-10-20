/* What `@gents/ux-sdk` resolves to inside a plugin's frame: the areas, the
   app's own primitives so a plugin's page reads like the app's, and the
   verbs that cross to the shell (`toast`). Everything a plugin may import
   is here, so the allowlist stays at three specifiers. The list is
   additive: a name exported once is never removed. The `ctx` a plugin
   receives is built per plugin by ./runtime.ts, not imported. */

/* areas and payload types */
export {
  AGENT_SECTIONS_AREA,
  KNOWN_AREAS,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@/contrib/types";
export type {
  AgentSectionData,
  AgentSectionProps,
  Contribution,
  ContributionSource,
  DirectiveChipData,
  HeaderActionData,
  KnownArea,
  NavRowData,
  TranscriptDirectiveProps,
} from "@/contrib/types";
export type {
  GentsUxPlugin,
  UxContext,
  UxContribution,
  UxData,
  UxDataCallResult,
  UxOs,
  UxShell,
  UxStorage,
} from "./contract";
export { useQuery, useWrite } from "./data-hooks";
export type { QueryState, UseQueryOptions } from "./data-hooks";
export type { Route } from "@/lib/router";

/* the app's own primitives */
export { pathFor as href } from "@/lib/router";
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

/* the shell's toaster, reached over the port: a frame has no toaster of its own */
export { toast } from "./toast";
