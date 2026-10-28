import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { NodeNavigationGuard } from "../src/ui/app/NodeNavigationGuard";
import { useFollowRoute } from "../src/ui/hooks/useFollowRoute";
import { href, interceptNavClicks, navigate, type Route } from "../src/ui/lib/router";
import { writeAttachments, writeDraft } from "../src/hooks/draftStore";
import { node, renderRouted, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const remote = "did:key:remote";
const target: Route = { name: "sessions", nodeDid: remote };
function Follow({ route }: { route: Route }) {
  useFollowRoute(route);
  return null;
}
function fixture() {
  const app = testApp({
    deployments: [
      node(),
      node({
        nodeDid: remote,
        source: "enrollment",
        label: "studio-1",
        node: { ...deployment.node, nodeDid: remote, displayName: "Amy" },
      }),
    ],
    selection: { nodeDid: deployment.nodeDid },
  });
  const key = app.project().draftKey;
  writeDraft(app.drafts, key, "Keep this seed on its original node");
  const rendered = renderRouted(app, { name: "session", sessionId: null }, (route) => (
    <TooltipProvider>
      <NodeNavigationGuard />
      <Follow route={route} />
    </TooltipProvider>
  ));
  return { app, key, ...rendered };
}

it("keeps the node and its draft when a switch is cancelled", async () => {
  const f = fixture();
  act(() => navigate(target));
  expect(screen.getByRole("alertdialog")).toHaveTextContent("Amy");
  expect(f.app.stores.selection.getState().nodeDid).toBe(deployment.nodeDid);
  expect(f.nav().route.name).toBe("session");
  fireEvent.click(screen.getByRole("button", { name: "Keep current node" }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(f.app.drafts.getState().drafts[f.key]).toContain("original node");
  expect(f.app.stores.selection.getState().nodeDid).toBe(deployment.nodeDid);
});

it("switches once after confirmation and preserves the source draft", async () => {
  const f = fixture();
  act(() => navigate(target));
  fireEvent.click(screen.getByRole("button", { name: "Switch node" }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(f.app.stores.selection.getState().nodeDid).toBe(remote);
  expect(f.app.drafts.getState().drafts[f.key]).toContain("original node");
  expect(f.app.drafts.getState().drafts[f.app.project().draftKey]).toBeUndefined();
});

it("does not apply a confirmation after the selection intent changes", () => {
  const f = fixture();
  act(() => navigate(target));
  act(() => f.app.actions.selectAgent("ops"));
  fireEvent.click(screen.getByRole("button", { name: "Switch node" }));
  expect(f.app.stores.selection.getState().nodeDid).toBe(deployment.nodeDid);
  expect(f.app.stores.selection.getState().agentId).toBe("ops");
});

it("does not ask about navigation within the active node", () => {
  const f = fixture();
  act(() =>
    navigate({ name: "agent", nodeDid: deployment.nodeDid, section: "profiles" }),
  );
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(f.nav().route.name).toBe("agent");
  expect(f.app.drafts.getState().drafts[f.key]).toContain("original node");
});

it("reviews attachment-only drafts and stops link side effects before switching", () => {
  const f = fixture();
  act(() => writeDraft(f.app.drafts, f.key, ""));
  act(() =>
    writeAttachments(f.app.drafts, f.key, () => [{ id: "capture", name: "AGENTS.md" }]),
  );
  const link = document.createElement("a");
  link.href = href(target);
  const sideEffect = vi.fn();
  link.addEventListener("click", sideEffect);
  document.body.append(link);
  const stop = interceptNavClicks();
  try {
    fireEvent.click(link);
    expect(screen.getByRole("alertdialog")).toBeVisible();
    expect(sideEffect).not.toHaveBeenCalled();
    expect(f.app.stores.selection.getState().nodeDid).toBe(deployment.nodeDid);
  } finally {
    stop();
    link.remove();
  }
});
