let last = 0;

/** The order of what this window asks the browser to show. Its commands run on
    threads of their own and can reach the native side out of order, which
    drops one older than the latest it applied (browser.rs, `in_order`).
    Counted in microseconds from the window's start, so a reloaded window's
    numbers stay above the last load's. */
export function nextSeq(): number {
  last = Math.max(
    last + 1,
    Math.floor((performance.timeOrigin + performance.now()) * 1000),
  );
  return last;
}
