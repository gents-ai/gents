//! The model-readable reference for `@gents/ux-sdk` and the custom properties
//! a plugin may use. Both files are generated from the desktop's frame by
//! `apps/gents-desktop/tests/ux-sdk-reference.test.ts`, which fails when they
//! stop matching its export namespace or its built stylesheet.

/// One page a model can write a UX plugin from: the page skeleton, every SDK
/// export with its props and a one-line example, and the theme tokens.
pub const SDK_REFERENCE: &str = include_str!("sdk_reference.md");

const FRAME_TOKENS: &str = include_str!("frame_tokens.txt");

/// The CSS custom properties the plugin frame defines for a plugin to use.
pub fn frame_tokens() -> impl Iterator<Item = &'static str> {
    FRAME_TOKENS.lines()
}
