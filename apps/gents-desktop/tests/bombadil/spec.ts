import { always, eventually } from "@antithesishq/bombadil";
import { extract } from "@antithesishq/bombadil/browser";
export * from "@antithesishq/bombadil/browser/defaults";

const shellState = extract((state) => {
  const document = state.document;
  const errorBanner = document.querySelector('[data-testid="error-banner"]');
  const surfaceSelectors = [
    '[data-testid="fleet-dashboard"]',
    '[data-testid="fleet-empty"]',
    '[data-testid="transcript-panel"]',
    ".config-workspace",
  ];
  return {
    shellMounted: Boolean(document.querySelector(".app-shell")),
    errorBanner: {
      visible: Boolean(errorBanner),
      message: normalizeText(
        errorBanner?.querySelector(".error-banner-message")?.textContent ?? "",
      ),
      isAlert: errorBanner?.getAttribute("role") === "alert",
      copyActionCount: errorBanner?.querySelectorAll("button.copy-button").length ?? 0,
      dismissActionCount:
        errorBanner?.querySelectorAll('[data-testid="error-banner-dismiss"]').length ??
        0,
    },
    documentWidth: Math.max(
      document.documentElement.scrollWidth,
      document.body?.scrollWidth ?? 0,
    ),
    viewportWidth: document.defaultView?.innerWidth ?? 0,
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

const tablistSelectionProblems = extract((state) => {
  return Array.from(state.document.querySelectorAll('[role="tablist"]'))
    .map((tablist) => {
      const tabs = Array.from(tablist.querySelectorAll('[role="tab"]'));
      const selected = tabs.filter(
        (tab) => tab.getAttribute("aria-selected") === "true",
      );
      const hasInteractiveControls = Boolean(
        tablist.querySelector("button, a, input, select, textarea, [tabindex]"),
      );
      return {
        label:
          tablist.getAttribute("aria-label") ??
          normalizeText(tablist.textContent ?? ""),
        hasInteractiveControls,
        tabCount: tabs.length,
        selectedCount: selected.length,
      };
    })
    .filter(
      (entry) =>
        (entry.hasInteractiveControls || entry.tabCount > 0) &&
        (entry.tabCount === 0 || entry.selectedCount !== 1),
    );
});

const tabPanelRelationshipProblems = extract((state) => {
  const issues: string[] = [];
  for (const tab of Array.from(state.document.querySelectorAll('[role="tab"]'))) {
    if (tab.getAttribute("aria-selected") !== "true") {
      continue;
    }
    const panelId = tab.getAttribute("aria-controls") ?? "";
    const tabId = tab.getAttribute("id") ?? "";
    const label = normalizeText(tab.textContent ?? tabId);
    if (!panelId) {
      issues.push(`${label}: selected tab is missing aria-controls`);
      continue;
    }
    const panel = state.document.getElementById(panelId);
    if (!panel) {
      issues.push(`${label}: controlled panel ${panelId} is missing`);
      continue;
    }
    if (panel.getAttribute("role") !== "tabpanel") {
      issues.push(`${label}: controlled element ${panelId} is not a tabpanel`);
    }
    if (tabId && panel.getAttribute("aria-labelledby") !== tabId) {
      issues.push(`${label}: panel ${panelId} does not point back to ${tabId}`);
    }
  }
  return issues;
});

const duplicateDomIds = extract((state) => {
  const counts = new Map<string, number>();
  for (const element of Array.from(state.document.querySelectorAll("[id]"))) {
    const id = element.getAttribute("id") ?? "";
    counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  return Array.from(counts.entries())
    .filter(([, count]) => count > 1)
    .map(([id, count]) => `${id} (${count})`);
});

const dialogProblems = extract((state) => {
  const dialogs = Array.from(state.document.querySelectorAll('[role="dialog"]'));
  const issues: string[] = [];
  if (dialogs.length > 1) {
    issues.push(`expected at most one dialog, found ${dialogs.length}`);
  }
  for (const dialog of dialogs) {
    if (!accessibleName(dialog, state.document)) {
      issues.push("dialog is missing an accessible name");
    }
    if (dialog.getAttribute("aria-modal") !== "true") {
      issues.push("dialog is missing aria-modal=true");
    }
  }
  return issues;
});

const emptyPrimarySurfaceProblems = extract((state) => {
  const surfaces = [
    '[data-testid="fleet-dashboard"]',
    '[data-testid="fleet-empty"]',
    '[data-testid="transcript-panel"]',
    ".config-workspace",
  ];
  return surfaces
    .map((selector) => {
      const surface = state.document.querySelector(selector);
      return {
        selector,
        mounted: Boolean(surface),
        textLength: normalizeText(surface?.textContent ?? "").length,
        declaresLoading: Boolean(
          surface?.querySelector('[role="status"], [aria-busy="true"]'),
        ),
      };
    })
    .filter((surface) => surface.mounted && surface.textLength === 0);
});

// Startup and Bombadil's Reload action can be observed before React mounts the
// shell and restores the route surface. Require both to settle promptly while
// still rejecting a persistently blank shell or overlapping primary surfaces.
export const desktop_shell_stays_mounted = always(
  eventually(() => shellState.current.shellMounted).within(5, "seconds"),
);

export const desktop_shell_has_one_primary_surface = always(
  eventually(() => shellState.current.primarySurfaceCount === 1).within(5, "seconds"),
);

export const desktop_global_errors_are_actionable = always(() => {
  const banner = shellState.current.errorBanner;
  return (
    !banner.visible ||
    (banner.message.length > 0 &&
      banner.isAlert &&
      banner.copyActionCount === 1 &&
      banner.dismissActionCount === 1)
  );
});

export const desktop_shell_does_not_horizontally_overflow = always(
  () => shellState.current.documentWidth <= shellState.current.viewportWidth + 2,
);

export const visible_tablists_have_one_selected_tab = always(
  () => tablistSelectionProblems.current.length === 0,
);

export const selected_tabs_control_visible_panels = always(
  () => tabPanelRelationshipProblems.current.length === 0,
);

export const desktop_shell_has_no_duplicate_dom_ids = always(
  () => duplicateDomIds.current.length === 0,
);

export const desktop_shell_does_not_stack_dialogs = always(
  () => dialogProblems.current.length === 0,
);

// A mounted primary surface with no text and no declared loading state
// ([role="status"] / [aria-busy="true"]) is blank — a rendering bug, caught
// immediately.
export const desktop_primary_surfaces_are_never_blank = always(
  () =>
    emptyPrimarySurfaceProblems.current.filter((surface) => !surface.declaresLoading)
      .length === 0,
);

// The transcript panel legitimately renders a text-free loading skeleton
// between selecting a session and its snapshot arriving (issue #996), so a
// continuous "never textually empty" assertion is wrong by design. What the
// app does guarantee is settling: a loading surface must produce content
// within the bound. A skeleton stuck past the bound (e.g. a dropped session
// subscription) still fails.
export const desktop_primary_surfaces_settle = always(
  eventually(() => emptyPrimarySurfaceProblems.current.length === 0).within(
    5,
    "seconds",
  ),
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

function accessibleName(element: Element, document: Document) {
  const ariaLabel = normalizeText(element.getAttribute("aria-label") ?? "");
  if (ariaLabel) {
    return ariaLabel;
  }
  const labelledBy = element.getAttribute("aria-labelledby") ?? "";
  return normalizeText(
    labelledBy
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" "),
  );
}
