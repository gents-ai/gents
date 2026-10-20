// The proof plugin: one row in every first-wave area, so the pipeline is
// exercised on every boot and a regression in any area shows up in the
// first screen. Ships OFF by default: it inventories in Agent ▸ UX Plugins
// and registers nothing until the switch is flipped. Plain ESM, loaded
// into a frame like every other plugin; its nav row opens its own page,
// the agent section is its configuration page.
import {
  AGENT_SECTIONS_AREA,
  Button,
  Group,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  Row,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@gents/ux-sdk";
import { jsx, jsxs } from "react/jsx-runtime";

function HelloSettings({ nodeDid, ctx }) {
  const visits = ctx.storage.get("visits", 0) + 1;
  ctx.storage.set("visits", visits);
  return jsxs(Group, {
    title: "Hello from a UX plugin",
    children: [
      jsx(Row, {
        label: "Node",
        description: "the DID this page was opened for",
        children: jsx("code", { className: "font-mono text-xs", children: nodeDid }),
      }),
      jsx(Row, {
        label: "Visits",
        description: "kept in the plugin's own storage namespace",
        children: String(visits),
      }),
      jsx(Row, {
        label: "Ask the agent",
        description: "posts a user turn into the selected session",
        children: jsx(Button, {
          size: "sm",
          onClick: () =>
            void ctx.send('Reply with exactly one line: ::hello{name="world"}'),
          children: "Send ::hello",
        }),
      }),
    ],
  });
}

function HelloPage() {
  return jsx(Group, {
    title: "Hello",
    children: jsx(Row, {
      label: "Page",
      description: "the plugin's own page, where its nav row goes",
      children: jsx("code", {
        className: "font-mono text-xs",
        children: "/plugins/hello",
      }),
    }),
  });
}

export default {
  id: "hello",
  name: "Hello",
  description:
    "A nav row, a page, an agent section, a header action and a ::hello directive.",
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
          icon: "sparkles",
          to: { name: "plugin", pluginId: "hello" },
          placement: "footer",
          order: 90,
        },
      },
      { id: "home", area: PLUGIN_PAGE_AREA, render: () => jsx(HelloPage, {}) },
      {
        id: "page",
        area: AGENT_SECTIONS_AREA,
        data: {
          group: "Packs",
          label: "Hello",
          icon: "sparkles",
          render: (props) => jsx(HelloSettings, { ...props, ctx }),
        },
      },
      {
        id: "header",
        area: SESSION_HEADER_ACTIONS_AREA,
        data: {
          label: "Hello plugin",
          icon: "sparkles",
          onClick: () => void ctx.send('::hello{name="desktop"}'),
        },
      },
      {
        id: "directive",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        data: { name: "hello", label: "Hello, {name}", icon: "sparkles" },
      },
    ]);
  },
};
