import { scrollParent } from "@gents/ui/conversation";

/* Hold something still across a layout change. Folding a long block away
   moves everything below it, and a reader who collapsed something ends up
   somewhere they never chose. Measure before the change, correct after it.

   Holding the top still is only right when that top is on screen. A reader
   deep inside a folding block has no anchor at all — what they were
   reading is gone — so the block itself is brought back into view rather
   than dropping them wherever the arithmetic lands, which is always below
   where they started.

   scrollParent comes from the kit, where the step's own hold already needed
   it: two copies of the same six lines is how they drift. */
/* how far the app's own chrome floats over the top of the scroller */
const inset = (node: HTMLElement) => {
  const declared = parseFloat(
    getComputedStyle(node).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};

/* call before the state change; call the result after it */
export function anchor(node: HTMLElement | null): () => void {
  const scroller = node && scrollParent(node);
  if (!node || !scroller) return () => {};
  const before = node.getBoundingClientRect().top;
  return () =>
    requestAnimationFrame(() => {
      const top = scroller.getBoundingClientRect().top + inset(node);
      const after = node.getBoundingClientRect().top;
      /* the reader was inside it: its top was above the view, so there is
         nothing of theirs left to hold. Put the block back under them. */
      if (before < top) scroller.scrollTop += after - top;
      else if (after !== before) scroller.scrollTop += after - before;
    });
}
