/* The source of the model-readable `@gents/ux-sdk` reference, which
   tests/ux-sdk-reference.test.ts renders to
   crates/gents/src/plugin/ux/sdk_reference.md. One entry per runtime export;
   `tsc` binds each block's props to its own props type, and the test binds
   the entries to the module and the tokens to the frame's built stylesheet.
   Only the test imports this, so none of it reaches the frame. */
import type { ComponentProps, ElementType } from "react";
import type {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
  Fact,
  Group,
  Hint,
  Input,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Row,
  ScrollArea,
  Separator,
  Skeleton,
  StackedRow,
  Switch,
  Textarea,
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "./sdk-exports";
import type {
  BarChartProps,
  DonutChartProps,
  EmptyStateProps,
  ErrorStateProps,
  GridProps,
  LastUpdatedProps,
  LineChartProps,
  LoadingStateProps,
  PageProps,
  SelectProps,
  StatProps,
  TableProps,
  TabsProps,
} from "./sdk-exports";

/* every prop of a block, or the build fails */
type AllProps<P> = { readonly [K in keyof Required<P>]: string };
/* the props worth a line on a kit component with the whole DOM behind it */
type SomeProps<P> = { readonly [K in keyof P]?: string };

export interface SdkEntry {
  section: string;
  /** a function's call shape, for an entry that is not a component */
  signature?: string;
  props?: Readonly<Record<string, string>>;
  example: string;
}

const SECTIONS = {
  layout: "Page layout",
  data: "Data blocks",
  charts: "Charts",
  states: "States",
  primitives: "Primitives",
  areas: "Contribution areas",
  hooks: "Data hooks",
  helpers: "Helpers",
} as const;

const area = (value: string, example: string): SdkEntry => ({
  section: SECTIONS.areas,
  signature: `the area id "${value}"`,
  example,
});

const part = <C extends ElementType>(
  props: SomeProps<ComponentProps<C>>,
  example: string,
): SdkEntry => ({
  section: SECTIONS.primitives,
  props: props as Readonly<Record<string, string>>,
  example,
});

const CHILDREN = "what goes inside";
const TRIGGER = "the element that opens it, as render: jsx(Button, {...})";

const PAGE: AllProps<PageProps> = {
  title: "the page heading",
  description: "one line under the heading",
  actions: "buttons at the right of the heading",
  children: "the page body; it scrolls with the page",
};
const GRID: AllProps<GridProps> = {
  children: "the cells",
  minWidth: "narrowest column in px; default 220, use 360 for charts",
};
const TABLE: AllProps<TableProps> = {
  columns:
    "field names, or { key, label, align: 'left'|'right', render(value, row) }; numbers align right on their own",
  rows: "array of objects",
  title: "heading above the table",
  rowKey: "the field that identifies a row",
  defaultSort: "{ key, direction: 'asc'|'desc' }",
  empty: "text when there are no rows",
  height: "the height it scrolls inside, in px; default 420",
};
const STAT: AllProps<StatProps> = {
  label: "what the number is",
  value: "a number (formatted for you) or text",
  delta: 'the change, as a number or text such as "+4% this week"',
  tone: "'neutral'|'good'|'warning'|'bad'; colors the delta",
};
const TABS: AllProps<TabsProps> = {
  items: "[{ value, label, content }]; a content's children stack with a gap",
  value: "the open tab; omit it to let the tabs keep their own",
  onChange: "(value) => void",
};
const SELECT: AllProps<SelectProps> = {
  options: "strings, or [{ value, label }]",
  value: "the chosen value",
  onChange: "(value) => void",
  placeholder: "shown while nothing is chosen",
  label: "names the control for a screen reader",
};
const LINE: AllProps<LineChartProps> = {
  data: "array of objects, one per point",
  x: "field along the bottom; dates are placed by time and sorted",
  y: "field to plot, or an array of up to 8 fields for several lines",
  title: "heading above the chart",
  height: "in px, axes included; default 220",
};
const BAR: AllProps<BarChartProps> = {
  data: "array of objects, one per category",
  x: "category field along the bottom",
  y: "field to plot, or an array of up to 8 fields for grouped bars",
  title: "heading above the chart",
  height: "in px, axes included; default 220",
};
const DONUT: AllProps<DonutChartProps> = {
  data: 'array of objects, one per slice; past 7, the smallest fold into "Other"',
  label: "field that names a slice",
  value: "field that sizes a slice",
  title: "heading above the chart",
  height: "in px; default 220",
};
const LOADING: AllProps<LoadingStateProps> = {
  label: 'what a screen reader hears; default "Loading"',
};
const EMPTY: AllProps<EmptyStateProps> = {
  title: 'default "Nothing to show"',
  message: "one line under the title",
  action: "a button out of the empty state",
};
const ERROR: AllProps<ErrorStateProps> = {
  message: "what went wrong; pass useQuery's error",
  title: 'default "Could not load this"',
  onRetry: "draws a Retry button; pass useQuery's refresh",
};
const LAST_UPDATED: AllProps<LastUpdatedProps> = {
  at: "a time; pass useQuery's updatedAt",
  loading: "says a read is running; pass useQuery's loading",
};

const BUTTON: SomeProps<ComponentProps<typeof Button>> = {
  variant:
    "'default'|'outline'|'secondary'|'ghost'|'quiet'|'destructive'|'brand'|'success'|'link'",
  size: "'default'|'xs'|'sm'|'lg'|'icon'|'icon-sm'",
  onClick: "click handler",
  disabled: "turns the button off",
  children: "the label",
};
const BADGE: SomeProps<ComponentProps<typeof Badge>> = {
  variant:
    "'default'|'outline'|'secondary'|'destructive'|'brand'|'success'|'lime'|'purple'|'yellow'",
  children: "the label",
};
const INPUT: SomeProps<ComponentProps<typeof Input>> = {
  value: "the text",
  onChange: "(event) => void",
  placeholder: "shown while empty",
  type: "the input type",
};
const CARD: SomeProps<ComponentProps<typeof Card>> = {
  size: "'default'|'sm'",
  children: "CardHeader, CardContent, CardFooter",
};
const DIALOG: SomeProps<ComponentProps<typeof Dialog>> = {
  open: "whether it is open; omit it to let the trigger manage it",
  onOpenChange: "(open) => void",
  children: "DialogTrigger and DialogContent",
};
const POPOVER: SomeProps<ComponentProps<typeof Popover>> = {
  open: "whether it is open; omit it to let the trigger manage it",
  onOpenChange: "(open) => void",
  children: "PopoverTrigger and PopoverContent",
};

export const SDK_ENTRIES: Readonly<Record<string, SdkEntry>> = {
  Page: {
    section: SECTIONS.layout,
    props: PAGE,
    example: "jsx(Page, { title: 'Tasks', children: body })",
  },
  Grid: {
    section: SECTIONS.layout,
    props: GRID,
    example: "jsxs(Grid, { children: [statA, statB, statC] })",
  },
  Table: {
    section: SECTIONS.data,
    props: TABLE,
    example:
      "jsx(Table, { columns: ['title', { key: 'count', label: 'Count' }], rows, defaultSort: { key: 'count', direction: 'desc' } })",
  },
  Stat: {
    section: SECTIONS.data,
    props: STAT,
    example: "jsx(Stat, { label: 'Open', value: 128, delta: '+4%', tone: 'good' })",
  },
  Tabs: {
    section: SECTIONS.data,
    props: TABS,
    example:
      "jsx(Tabs, { items: [{ value: 'a', label: 'All', content: all }, { value: 'b', label: 'Late', content: late }] })",
  },
  Select: {
    section: SECTIONS.data,
    props: SELECT,
    example:
      "jsx(Select, { options: ['ready', 'done'], value: lane, onChange: setLane, label: 'Lane' })",
  },
  LineChart: {
    section: SECTIONS.charts,
    props: LINE,
    example: "jsx(LineChart, { title: 'Created', data: rows, x: 'day', y: 'count' })",
  },
  BarChart: {
    section: SECTIONS.charts,
    props: BAR,
    example: "jsx(BarChart, { title: 'By lane', data: rows, x: 'lane', y: 'count' })",
  },
  DonutChart: {
    section: SECTIONS.charts,
    props: DONUT,
    example:
      "jsx(DonutChart, { title: 'Share', data: rows, label: 'lane', value: 'count' })",
  },
  LoadingState: {
    section: SECTIONS.states,
    props: LOADING,
    example: "if (q.loading && !q.data) return jsx(LoadingState, {})",
  },
  EmptyState: {
    section: SECTIONS.states,
    props: EMPTY,
    example: "jsx(EmptyState, { title: 'No tasks yet', message: 'Add one to start.' })",
  },
  ErrorState: {
    section: SECTIONS.states,
    props: ERROR,
    example: "jsx(ErrorState, { message: q.error, onRetry: q.refresh })",
  },
  LastUpdated: {
    section: SECTIONS.states,
    props: LAST_UPDATED,
    example: "jsx(LastUpdated, { at: q.updatedAt, loading: q.loading })",
  },
  Button: {
    section: SECTIONS.primitives,
    props: BUTTON,
    example: "jsx(Button, { size: 'sm', onClick: save, children: 'Save' })",
  },
  Badge: {
    section: SECTIONS.primitives,
    props: BADGE,
    example: "jsx(Badge, { variant: 'success', children: 'Live' })",
  },
  Input: {
    section: SECTIONS.primitives,
    props: INPUT,
    example: "jsx(Input, { value: text, onChange: (e) => setText(e.target.value) })",
  },
  Textarea: part<typeof Textarea>(
    {
      value: "the text",
      onChange: "(event) => void",
      placeholder: "shown while empty",
      rows: "visible lines",
    },
    "jsx(Textarea, { value: text, onChange: (e) => setText(e.target.value) })",
  ),
  Switch: part<typeof Switch>(
    {
      checked: "on or off",
      onCheckedChange: "(checked) => void",
      disabled: "turns it off",
      size: "'default'|'sm'",
    },
    "jsx(Switch, { checked: on, onCheckedChange: setOn })",
  ),
  Separator: part<typeof Separator>(
    { orientation: "'horizontal'|'vertical'" },
    "jsx(Separator, {})",
  ),
  ScrollArea: part<typeof ScrollArea>(
    {
      orientation: "'vertical'|'horizontal'|'both'; default vertical",
      style: "give it a height, or it will not scroll",
      children: CHILDREN,
    },
    "jsx(ScrollArea, { style: { height: 240 }, children: list })",
  ),
  Skeleton: part<typeof Skeleton>(
    { style: "its size, as { height, width }" },
    "jsx(Skeleton, { style: { height: 16, width: 120 } })",
  ),
  Card: {
    section: SECTIONS.primitives,
    props: CARD,
    example: "jsxs(Card, { children: [jsx(CardContent, { children: body })] })",
  },
  CardHeader: part<typeof CardHeader>(
    { children: "CardTitle and CardDescription" },
    "jsx(CardHeader, { children: jsx(CardTitle, { children: 'Title' }) })",
  ),
  CardTitle: part<typeof CardTitle>(
    { children: "the title" },
    "jsx(CardTitle, { children: 'Title' })",
  ),
  CardDescription: part<typeof CardDescription>(
    { children: "one line of text" },
    "jsx(CardDescription, { children: 'One line' })",
  ),
  CardContent: part<typeof CardContent>(
    { children: CHILDREN },
    "jsx(CardContent, { children: body })",
  ),
  CardFooter: part<typeof CardFooter>(
    { children: "usually buttons" },
    "jsx(CardFooter, { children: buttons })",
  ),
  Dialog: {
    section: SECTIONS.primitives,
    props: DIALOG,
    example:
      "jsxs(Dialog, { children: [jsx(DialogTrigger, { render: jsx(Button, { children: 'Open' }) }), jsx(DialogContent, { children: body })] })",
  },
  DialogTrigger: part<typeof DialogTrigger>(
    { render: TRIGGER },
    "jsx(DialogTrigger, { render: jsx(Button, { children: 'Open' }) })",
  ),
  DialogContent: part<typeof DialogContent>(
    { children: CHILDREN, showCloseButton: "draws the corner X; default true" },
    "jsx(DialogContent, { children: body })",
  ),
  DialogHeader: part<typeof DialogHeader>(
    { children: "DialogTitle and DialogDescription" },
    "jsx(DialogHeader, { children: jsx(DialogTitle, { children: 'Title' }) })",
  ),
  DialogTitle: part<typeof DialogTitle>(
    { children: "the title" },
    "jsx(DialogTitle, { children: 'Title' })",
  ),
  DialogDescription: part<typeof DialogDescription>(
    { children: "one line of text" },
    "jsx(DialogDescription, { children: 'One line' })",
  ),
  DialogFooter: part<typeof DialogFooter>(
    { children: "usually buttons" },
    "jsx(DialogFooter, { children: buttons })",
  ),
  Popover: {
    section: SECTIONS.primitives,
    props: POPOVER,
    example:
      "jsxs(Popover, { children: [jsx(PopoverTrigger, { render: jsx(Button, { children: 'More' }) }), jsx(PopoverContent, { children: body })] })",
  },
  PopoverTrigger: part<typeof PopoverTrigger>(
    { render: TRIGGER },
    "jsx(PopoverTrigger, { render: jsx(Button, { children: 'More' }) })",
  ),
  PopoverContent: part<typeof PopoverContent>(
    {
      children: CHILDREN,
      side: "'top'|'bottom'|'left'|'right'",
      align: "'start'|'center'|'end'",
    },
    "jsx(PopoverContent, { children: body })",
  ),
  Tooltip: part<typeof Tooltip>(
    { children: "TooltipTrigger and TooltipContent" },
    "jsxs(Tooltip, { children: [jsx(TooltipTrigger, { render: el }), jsx(TooltipContent, { children: 'Hint' })] })",
  ),
  TooltipTrigger: part<typeof TooltipTrigger>(
    { render: "the element the tooltip is about" },
    "jsx(TooltipTrigger, { render: el })",
  ),
  TooltipContent: part<typeof TooltipContent>(
    { children: "the tooltip text", side: "'top'|'bottom'|'left'|'right'" },
    "jsx(TooltipContent, { children: 'Hint' })",
  ),
  TooltipProvider: part<typeof TooltipProvider>(
    { delay: "ms before a tooltip opens", children: "the tooltips it governs" },
    "jsx(TooltipProvider, { delay: 600, children: tooltips })",
  ),
  Hint: part<typeof Hint>(
    {
      label: "the tooltip text",
      keys: "a keyboard shortcut to show beside it",
      children: "the one element it is about",
    },
    "jsx(Hint, { label: 'Refresh', children: jsx(Button, { children: 'R' }) })",
  ),
  Group: part<typeof Group>(
    {
      title: "the group's heading",
      action: "a control at the right of the heading",
      children: "Row and StackedRow",
    },
    "jsx(Group, { title: 'Settings', children: rows })",
  ),
  Row: part<typeof Row>(
    {
      label: "what the row is",
      description: "one line under the label",
      htmlFor: "the id of the control it labels",
      error: "an error under the label",
      children: "the control or value at the right",
    },
    "jsx(Row, { label: 'Name', description: 'Shown to everyone', children: control })",
  ),
  StackedRow: part<typeof StackedRow>(
    {
      label: "what the row is",
      description: "one line under the label",
      htmlFor: "the id of the control it labels",
      error: "an error under the label",
      action: "a control at the right of the label",
      children: "the control below the label",
    },
    "jsx(StackedRow, { label: 'Notes', children: textarea })",
  ),
  Fact: part<typeof Fact>(
    {
      children: "the value; empty shows a dash",
      mono: "monospace, for ids and paths",
      title: "the full text on hover",
    },
    "jsx(Fact, { mono: true, children: 'value' })",
  ),
  NAV_AREA: area(
    "nav",
    "{ id: 'nav', area: NAV_AREA, data: { id: 'x:nav', label: 'X', icon: 'puzzle', to: { name: 'plugin', pluginId: 'x' }, placement: 'footer' } }",
  ),
  PLUGIN_PAGE_AREA: area(
    "plugin.pages",
    "{ id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(MyPage, { ctx }) }",
  ),
  AGENT_SECTIONS_AREA: area(
    "agent.sections",
    "{ id: 'sec', area: AGENT_SECTIONS_AREA, data: { group: 'Configure', label: 'X', render: (props) => jsx(Section, props) } }",
  ),
  SESSION_HEADER_ACTIONS_AREA: area(
    "session.header.actions",
    "{ id: 'act', area: SESSION_HEADER_ACTIONS_AREA, data: { label: 'X', onClick: run } }",
  ),
  TRANSCRIPT_DIRECTIVE_AREA: area(
    "transcript.directives",
    "{ id: 'dir', area: TRANSCRIPT_DIRECTIVE_AREA, data: { name: 'x', label: 'X {id}', onClick: open } }",
  ),
  KNOWN_AREAS: {
    section: SECTIONS.areas,
    signature: "the list of every area id",
    example: "KNOWN_AREAS.includes(area)",
  },
  useQuery: {
    section: SECTIONS.hooks,
    signature:
      "useQuery(ctx, surface, tool, options?) -> { data, error, loading, updatedAt, refresh }. options: { args, live, intervalMs, enabled, pages: 'one'|'all', maxRows, recoverFields }. data is the tool's answer: { results, count, next_offset? }.",
    example: "const q = useQuery(ctx, 'board', 'list_tasks', { pages: 'all' })",
  },
  useWrite: {
    section: SECTIONS.hooks,
    signature: "useWrite(ctx, surface, tool) -> { write(args), pending, error }",
    example:
      "const add = useWrite(ctx, 'board', 'add_task'); await add.write({ title })",
  },
  cn: {
    section: SECTIONS.helpers,
    signature: "cn(...classNames) -> string",
    example: "cn('truncate', active && 'font-medium')",
  },
  href: {
    section: SECTIONS.helpers,
    signature: "href(route) -> the path of an app route",
    example: "href({ name: 'plugin', pluginId: 'x' })",
  },
  toast: {
    section: SECTIONS.helpers,
    signature: "toast(message), toast.success, toast.error, toast.info, toast.warning",
    example: "toast.success('Saved')",
  },
};

/* what a color name is for; every other token is listed by name alone */
export const TOKEN_ROLES: Readonly<Record<string, string>> = {
  "--bg": "the page ground",
  "--surface": "one step up from the ground: inset fields, code",
  "--raised": "one step up from surface",
  "--card": "a card's background",
  "--card-foreground": "text on a card",
  "--popover": "a popover's background",
  "--popover-foreground": "text in a popover",
  "--text": "body text",
  "--text-muted": "secondary text",
  "--muted-foreground": "secondary text (same as --text-muted)",
  "--heading": "headings",
  "--border": "hairlines and dividers",
  "--input": "form field fill",
  "--ring": "focus outline",
  "--muted": "a quiet fill",
  "--primary": "the default button's fill",
  "--primary-foreground": "text on --primary",
  "--secondary": "a neutral fill",
  "--secondary-foreground": "text on --secondary",
  "--accent": "hover fill",
  "--accent-foreground": "text on --accent",
  "--brand": "the one standout action on a screen",
  "--brand-foreground": "text on --brand",
  "--destructive": "errors and danger",
  "--warning": "nearly at a limit",
  "--success": "a positive fill",
  "--success-foreground": "positive text",
  "--chart-1": "first series; --chart-2 to --chart-8 follow in order",
};

const SKELETON = `import { NAV_AREA, PLUGIN_PAGE_AREA, Grid, Page, Stat, useQuery } from '@gents/ux-sdk'
import { jsx, jsxs } from 'react/jsx-runtime'

function Dashboard({ ctx }) {
  const q = useQuery(ctx, 'my_surface', 'list_items', { pages: 'all' })
  const rows = q.data?.results ?? []
  return jsx(Page, {
    title: 'Items',
    children: jsxs(Grid, { children: [jsx(Stat, { label: 'Items', value: rows.length })] }),
  })
}

export default {
  id: 'my_plugin',
  name: 'My plugin',
  register(ctx) {
    ctx.registerMany([
      { id: 'nav', area: NAV_AREA, data: { id: 'my_plugin:nav', label: 'My plugin', icon: 'puzzle', to: { name: 'plugin', pluginId: 'my_plugin' }, placement: 'footer' } },
      { id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Dashboard, { ctx }) },
    ])
  },
}`;

/** how a model asks for one export's section, or the token list */
const ASK = '["help","page","sdk",NAME]';
/** ends the index; one `### NAME` section per export, then `### tokens`, follow */
const SECTIONS_MARKER =
  "<!-- Generated by apps/gents-desktop/tests/ux-sdk-reference.test.ts. Do not edit. Sections follow. -->";
/** the token names a page needs most, kept in the index */
const COMMON_TOKENS = [
  "--bg",
  "--surface",
  "--card",
  "--text",
  "--text-muted",
  "--heading",
  "--border",
  "--primary",
  "--brand",
  "--destructive",
  "--warning",
  "--success",
  "--chart-1",
] as const;

/* an export on one index line: its prop names, or its call shape */
function indexLine(name: string, entry: SdkEntry): string {
  if (entry.props) return `- \`${name}\`: ${Object.keys(entry.props).join(", ")}`;
  const call = (entry.signature ?? "").split(". ")[0]!;
  return call.startsWith(`${name}(`) ? `- \`${call}\`` : `- \`${name}\`: ${call}`;
}

/** The reference, as checked in. A model reads the index above
    SECTIONS_MARKER first, then one section per name it asks for. */
export function renderSdkReference(tokens: readonly string[]): string {
  const lines = [
    "# @gents/ux-sdk",
    "",
    "A UX plugin is one plain ESM file, no build step and no JSX. It may import only",
    "`@gents/ux-sdk`, `react` and `react/jsx-runtime`, and writes elements with",
    "`jsx(type, props)` and `jsxs(type, { children: [...] })`.",
    "Prefer these blocks to your own CSS: a plugin's Tailwind classes are not compiled.",
    "Declare the surfaces and tools a page calls in `data` on its page document, or under `ux[].data` in a pack manifest.",
    "",
    "## Page skeleton",
    "",
    "```js",
    SKELETON,
    "```",
    "",
    "`ctx` is passed to `register`; it also offers `ctx.register(one)`, `ctx.storage`,",
    "`ctx.shell`, `ctx.data` and `ctx.send(text)`.",
    "",
    "## Exports",
    "",
    `Each line is an export with its props or call shape. ${ASK} gives one export's`,
    "prop meanings and an example; several names may follow `sdk`.",
    "",
  ];
  for (const section of Object.values(SECTIONS)) {
    lines.push(`${section}:`);
    for (const [name, entry] of Object.entries(SDK_ENTRIES)) {
      if (entry.section === section) lines.push(indexLine(name, entry));
    }
    lines.push("");
  }
  for (const token of COMMON_TOKENS) {
    if (!tokens.includes(token)) throw new Error(`${token} is not a frame token`);
  }
  lines.push(
    "## Tokens",
    "",
    "`gents config pack ux lint` refuses raw hex, rgb() and hsl() colors, and any custom property",
    "the frame does not define and the plugin does not define itself. Frame tokens",
    "follow the theme, so use them instead of a color: `var(--text)`, never",
    "`--background` or `--foreground`, which do not exist here. Most used:",
    `${COMMON_TOKENS.map((token) => `\`${token}\``).join(", ")} (to \`--chart-8\`).`,
    `${ASK.replace("NAME", '"tokens"')} lists every token and its role.`,
    "",
    SECTIONS_MARKER,
    "",
  );
  for (const [name, entry] of Object.entries(SDK_ENTRIES)) {
    lines.push(`### ${name}`, "");
    if (entry.signature) lines.push(entry.signature, "");
    for (const [prop, doc] of Object.entries(entry.props ?? {})) {
      lines.push(`- \`${prop}\`: ${doc}`);
    }
    if (entry.props) lines.push("");
    lines.push(`\`${entry.example}\``, "");
  }
  const rest = tokens.filter((token) => !(token in TOKEN_ROLES));
  lines.push("### tokens", "");
  for (const [token, role] of Object.entries(TOKEN_ROLES)) {
    lines.push(`- \`${token}\`: ${role}`);
  }
  lines.push(
    "",
    `Also defined: ${rest.map((token) => `\`${token}\``).join(", ")}.`,
    "",
  );
  return lines.join("\n");
}
