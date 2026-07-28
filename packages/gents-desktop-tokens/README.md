# @source-inc/gents-desktop-tokens

Semantic CSS custom properties for packaged Gents desktop UI.

- **Semantic** (`semantic.css`): `--color-bg`, `--color-surface`, `--color-text`, `--color-accent`, spacing, radii, fonts.
- **Brand** (`--source-green`, logos, product fonts): stays in the host app.

The published defaults are deliberately neutral greyscale values, not the
Source palette. Packaged components only reference semantic vars; hosts map
their own palettes onto those slots after importing the package styles.
