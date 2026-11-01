/** The composer's text field: no macOS autocorrect, capitalising or
    predictive completions over what is typed, since a message is often code,
    names or commands; spelling is still underlined. */
export const COMPOSER_FIELD = {
  autoCorrect: "off",
  autoCapitalize: "off",
  /* WebKit's inline predictions; React passes the attribute through as is */
  writingsuggestions: "false",
} as Record<string, string>;
