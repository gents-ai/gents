# `@source-inc/gents-desktop-ui`

Accessible, host-neutral primitives shared by the Gents desktop surfaces.

Import the semantic token contract before the package stylesheet:

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
```

The JavaScript entry point exports `CopyButton`, `ConfirmDialog`, `copyText`,
and `formatMessageTime`. The stylesheet also owns the primitive classes emitted
by the domain packages: buttons, panels, fields, chips, and typography
utilities. Components rely only on semantic CSS custom properties.

Complete host CSS order:

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
@import "@source-inc/gents-desktop-chat/styles.css"; /* as used */
@import "@source-inc/gents-desktop-fleet/styles.css";
@import "@source-inc/gents-desktop-operations/styles.css";
/* Define host semantic-token overrides after the package imports. */
```
