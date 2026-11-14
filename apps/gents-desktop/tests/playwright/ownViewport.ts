/** Marks a test that sets its own window size before it first navigates, in
its details (`{ tag: OWN_VIEWPORT }`) or at the end of its title. The
three Chromium projects differ only in window size, so such a test runs on
chromium-desktop alone (chromium-laptop and chromium-narrow exclude the tag in
playwright.config.ts); WebKit still runs it where its testMatch names the
file. Wherever it runs, its page starts at PROJECT_SIZE_SENTINEL instead of
the project's size, and the test fails if it navigates or reads
`page.viewportSize()` before setting its own size: a tagged test cannot come
to depend on the size of the project it runs in without failing. */
export const OWN_VIEWPORT = "@own-viewport";

/** No project uses this size; tests/playwright-fixture-guard.test.ts checks that. */
export const PROJECT_SIZE_SENTINEL = { width: 1013, height: 677 };
