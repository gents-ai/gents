/** No macOS autocorrect, capitalising or predictive completions over what
    is typed. A correction bubble also takes ↑ and ↓ from a field whose list
    is driven by them. */
export const NO_TEXT_ASSIST = {
  autoCorrect: "off",
  autoCapitalize: "off",
  /* WebKit's inline predictions; React passes the attribute through as is */
  writingsuggestions: "false",
} as Record<string, string>;

/** A search or filter field: no text assistance, and no spelling marks or
    remembered entries, since a query is names and fragments, not prose. */
export const SEARCH_FIELD = {
  ...NO_TEXT_ASSIST,
  spellCheck: "false",
  autoComplete: "off",
} as Record<string, string>;
