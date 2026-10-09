import { describe, expect, it } from "vitest";
import { exactControl, labelledInput, persistedSessionControl } from "./nativeLocalE2e";

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
