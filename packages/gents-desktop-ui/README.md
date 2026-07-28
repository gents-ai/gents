# `@source-inc/gents-desktop-ui`

Accessible, host-neutral primitives shared by the Gents desktop surfaces.

Import the semantic token contract before the package stylesheet:

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
```

The JavaScript entry point exports `CopyButton`, `ConfirmDialog`, `copyText`,
and `formatMessageTime`. Components rely only on semantic CSS custom
properties; hosts remain responsible for mapping their brand palette into
those slots.
