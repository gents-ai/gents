import { always } from "@antithesishq/bombadil";
import { extract } from "@antithesishq/bombadil/browser";
export * from "@antithesishq/bombadil/browser/defaults";

const shellState = extract((state) => {
  const document = state.document;
  const surfaceSelectors = [
    '[data-testid="fleet-dashboard"]',
    '[data-testid="fleet-empty"]',
    '[data-testid="transcript-panel"]',
    ".config-workspace",
  ];
  return {
    shellMounted: Boolean(document.querySelector(".app-shell")),
    errorBanner:
      document.querySelector('[data-testid="error-banner"]')?.textContent ?? "",
    primarySurfaceCount: surfaceSelectors.filter((selector) =>
      document.querySelector(selector),
    ).length,
  };
});

const transcriptCards = extract((state) => {
  return Array.from(
    state.document.querySelectorAll('[data-testid="transcript-panel"] .message-card'),
  ).map((card) => {
    const role = normalizeText(card.querySelector(".message-role")?.textContent ?? "");
    const content = normalizeText(
      card.querySelector(".message-content")?.textContent ?? "",
    );
    return { role, content };
  });
});

const unnamedButtons = extract((state) => {
  return Array.from(state.document.querySelectorAll("button"))
    .filter((button) => !button.disabled)
    .map((button) => {
      const label =
        button.getAttribute("aria-label") ??
        button.getAttribute("title") ??
        button.textContent ??
        "";
      return normalizeText(label);
    })
    .filter((label) => label.length === 0);
});

export const desktop_shell_stays_mounted = always(
  () => shellState.current.shellMounted,
);

export const desktop_shell_has_one_primary_surface = always(
  () => shellState.current.primarySurfaceCount === 1,
);

export const desktop_shell_does_not_show_global_errors = always(
  () => shellState.current.errorBanner.length === 0,
);

export const transcript_does_not_render_adjacent_duplicate_messages = always(() => {
  const rows = transcriptCards.current;
  for (let index = 1; index < rows.length; index += 1) {
    const previous = rows[index - 1];
    const current = rows[index];
    if (
      previous.role &&
      current.role &&
      previous.content &&
      current.content &&
      previous.role === current.role &&
      previous.content === current.content
    ) {
      return false;
    }
  }
  return true;
});

export const enabled_buttons_have_accessible_names = always(
  () => unnamedButtons.current.length === 0,
);

function normalizeText(value: string) {
  return value.replace(/\s+/g, " ").trim();
}
