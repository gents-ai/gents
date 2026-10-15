import { describe, expect, it } from "vitest";

import { restoreListViews } from "../src/ui/app/listViews";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { restorePreferences, type Preferences } from "../src/ui/preferences";

const defaults: Preferences = { theme: "light", nav: "hover" };

describe("preferences read back from storage", () => {
  it("keeps the defaults for values that are not preferences", () => {
    expect(restorePreferences({ theme: "sepia", nav: 3 }, defaults)).toEqual(defaults);
    expect(restorePreferences(null, defaults)).toEqual(defaults);
    expect(restorePreferences({ theme: "dark", nav: "collapsed" }, defaults)).toEqual({
      theme: "dark",
      nav: "collapsed",
    });
  });
});

describe("list views read back from storage", () => {
  /* an earlier build offered "Needs you"; its saved pick must not be read
     as another state */
  it("drops a session state or source this build does not offer", () => {
    const restored = restoreListViews({
      sessionFilter: {
        states: ["held", "live"],
        sources: ["robot", "task"],
        agents: [],
      },
    });
    expect(restored.sessionFilter).toEqual({
      states: ["live"],
      sources: ["task"],
      agents: [],
    });
  });

  it("leaves out what is not a list of strings, and keeps an empty node pick", () => {
    expect(restoreListViews({ sessionNodes: "a", mailboxNodes: ["a", 1] })).toEqual({
      sessionFilter: emptyFilter,
      sessionNodes: null,
      mailboxNodes: ["a"],
      mailboxKinds: [],
    });
    expect(restoreListViews({ sessionNodes: [] }).sessionNodes).toEqual([]);
  });
});
