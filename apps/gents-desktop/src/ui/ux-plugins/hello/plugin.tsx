/* The proof plugin: one row in every first-wave area, so the pipeline is
   exercised on every boot and a regression in any area shows up in the
   first screen. Ships OFF by default: it inventories in Agent ▸ UX Plugins
   and registers nothing until the switch is flipped. Mirrors hermes-agent's
   hello-runtime, which proves the same pipeline there. */
import { Sparkles } from "lucide-react";
import {
  AGENT_SECTIONS_AREA,
  Button,
  Group,
  Hint,
  NAV_AREA,
  Row,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  type AgentSectionData,
  type AgentSectionProps,
  type GentsUxPlugin,
  type NavItem,
  type TranscriptDirectiveContribution,
  type UxContext,
} from "@gents/ux-sdk";

function HelloPage({ agentDid, ctx }: AgentSectionProps & { ctx: UxContext }) {
  const visits = ctx.storage.get<number>("visits", 0) + 1;
  ctx.storage.set("visits", visits);
  return (
    <Group title="Hello from a UX plugin">
      <Row label="Agent" description="the DID this page was opened for">
        <code className="font-mono text-xs">{agentDid}</code>
      </Row>
      <Row label="Visits" description="kept in the plugin's own storage namespace">
        {visits}
      </Row>
      <Row
        label="Ask the agent"
        description="posts a hidden user turn into the selected session"
      >
        <Button
          size="sm"
          onClick={() =>
            void ctx.send('Reply with exactly one line: ::hello{name="world"}')
          }
        >
          Send ::hello
        </Button>
      </Row>
    </Group>
  );
}

function HelloDirective({ attrs }: { attrs: Readonly<Record<string, string>> }) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-xs"
      data-testid="hello-directive"
    >
      <Sparkles className="size-3" />
      Hello, {attrs.name ?? "there"}
    </span>
  );
}

const plugin: GentsUxPlugin = {
  id: "hello",
  name: "Hello",
  description: "A nav row, an agent section, a header action and a ::hello directive.",
  defaultEnabled: false,
  register(ctx) {
    ctx.registerMany([
      {
        id: "nav",
        area: NAV_AREA,
        order: 90,
        data: {
          id: "hello:nav",
          label: "Hello",
          icon: <Sparkles className="size-4" />,
          to: { name: "agents" },
          active: () => false,
          placement: "footer",
          order: 90,
        } satisfies NavItem,
      },
      {
        id: "page",
        area: AGENT_SECTIONS_AREA,
        data: {
          group: "Packs",
          label: "Hello",
          icon: <Sparkles className="size-4" />,
          render: (props: AgentSectionProps) => <HelloPage {...props} ctx={ctx} />,
        } satisfies AgentSectionData,
      },
      {
        id: "header",
        area: SESSION_HEADER_ACTIONS_AREA,
        render: () => (
          <Hint label="Hello plugin">
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Hello plugin"
              onClick={() => void ctx.send('::hello{name="desktop"}')}
            >
              <Sparkles />
            </Button>
          </Hint>
        ),
      },
      {
        id: "directive",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        data: {
          name: "hello",
          render: ({ attrs }) => <HelloDirective attrs={attrs} />,
        } satisfies TranscriptDirectiveContribution,
      },
    ]);
  },
};

export default plugin;
