import { describe, expect, it } from "vitest";
import {
  exactControl,
  emulateNativeReducedMotion,
  labelledInput,
  persistedSessionControl,
  renderedAssistantResponse,
} from "./nativeLocalE2e";
import { findAssistantResponseMarker } from "./nativeSimulatorE2eDom";

it("emulates reduced motion only in the native test window", () => {
  const target = {
    matchMedia: (media: string) => ({ media, matches: false }) as MediaQueryList,
  };
  emulateNativeReducedMotion(target);
  expect(target.matchMedia("(prefers-reduced-motion: reduce)").matches).toBe(true);
  expect(target.matchMedia("(prefers-color-scheme: dark)").matches).toBe(false);
});

describe("native canonical response matching", () => {
  it("matches the full rendered answer when canonical Markdown contains formatting", () => {
    const response = "Both received: `QUEUE_SECOND` and **QUEUE_EDITED**.";
    document.body.innerHTML =
      '<div data-slot="assistant-message">Both received: <code>QUEUE_SECOND</code> and <strong>QUEUE_EDITED</strong>.</div>';
    expect(findAssistantResponseMarker(document, response)).toBeNull();
    expect(renderedAssistantResponse(response)).toBe(
      "Both received: QUEUE_SECOND and QUEUE_EDITED.",
    );
    expect(
      findAssistantResponseMarker(document, renderedAssistantResponse(response)),
    ).not.toBeNull();
  });

  it("still rejects a rendered answer with a missing retained message", () => {
    document.body.innerHTML =
      '<div data-slot="assistant-message">Both received: <code>QUEUE_SECOND</code>.</div>';
    expect(
      findAssistantResponseMarker(
        document,
        renderedAssistantResponse("Both received: `QUEUE_SECOND` and `QUEUE_EDITED`."),
      ),
    ).toBeNull();
  });
});

describe("native local setup selectors", () => {
  it("finds both nested setup fields and the separately labelled tool root", () => {
    document.body.innerHTML = `
      <label><span>Node name</span><input id="name"></label>
      <label for="root">Tool root</label><input id="root">
      <label><span>Endpoint</span><input id="endpoint"></label>`;
    expect(labelledInput(document, "Node name")?.id).toBe("name");
    expect(labelledInput(document, "Tool root")?.id).toBe("root");
    expect(labelledInput(document, "Endpoint")?.id).toBe("endpoint");
  });

  it("selects the exact enabled Engineer option instead of a similarly named agent", () => {
    document.body.innerHTML = `
      <button role="option"><span>The Engineer staging</span></button>
      <button role="option" disabled><span>The Engineer</span></button>
      <button role="option" id="engineer"><span>The Engineer<span>default</span></span><span>GLM model</span></button>`;
    expect(exactControl(document, '[role="option"]', "The Engineer")?.id).toBe(
      "engineer",
    );
    expect(exactControl(document, '[role="option"]', "Missing")).toBeNull();
  });

  it("opens the session list when restart restores an existing conversation directly", () => {
    document.body.innerHTML = `<main data-testid="session-screen"><a href="#/sessions">Sessions</a></main>`;
    expect(persistedSessionControl(document, "saved")?.kind).toBe("list");
    document.body.innerHTML = `<a data-testid="session-another">Another</a><a data-testid="session-saved">Saved</a>`;
    const control = persistedSessionControl(document, "saved");
    expect(control?.kind).toBe("session");
    expect(control?.element.textContent).toBe("Saved");
  });
});
