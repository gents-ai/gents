import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { BackendEditor } from "../src/ui/screens/agent/BackendEditor";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

describe("a provider's usage on its own page", () => {
  it("holds the meters' shape while the first read is out, in bones that show on the panel", () => {
    const backend = deployment.inferenceBackends[0]!;
    const app = testApp({ deployments: [deployment] });
    const editor = (loading: boolean) => (
      <BackendEditor
        deployment={deployment}
        backend={backend}
        accounts={[]}
        usage={{ loading, refresh: vi.fn() }}
      />
    );
    const view = renderIn(app, editor(true));
    const busy = document.querySelector('[aria-busy="true"]');
    expect(busy).not.toBeNull();
    expect(busy!.querySelector('[data-slot="skeleton"]')).toHaveClass(
      "bg-foreground/10",
    );
    expect(screen.queryByText("Usage unknown")).toBeNull();

    view.rerender(editor(false));
    expect(document.querySelector('[aria-busy="true"]')).toBeNull();
    expect(screen.getByText("Usage unknown")).toBeInTheDocument();
  });
});
