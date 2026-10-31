import { flushSync } from "react-dom";

/** Applies `update` as a view transition: a card named for one (a
    `view-transition-name`, with the `morph-card` class) animates from its old
    size to its new one while its contents cross-fade (morph.css), and the
    rest of the page holds still. Without the API, or with reduced motion,
    `update` simply applies. */
export function morph(update: () => void) {
  const doc = document as Document & {
    startViewTransition?: (callback: () => void) => unknown;
  };
  if (
    !doc.startViewTransition ||
    matchMedia("(prefers-reduced-motion: reduce)").matches
  )
    return update();
  const root = document.documentElement;
  root.dataset.morph = "";
  const transition = doc.startViewTransition(() => flushSync(update)) as {
    finished?: Promise<unknown>;
  };
  void Promise.resolve(transition?.finished).finally(() => delete root.dataset.morph);
}
