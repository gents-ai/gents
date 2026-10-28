export function scrollRoom(start: Element | null): { left: boolean; right: boolean } {
  let left = false;
  let right = false;
  for (let el = start; el && el !== document.documentElement; el = el.parentElement) {
    if (el.scrollWidth <= el.clientWidth + 1) continue;
    const overflow = getComputedStyle(el).overflowX;
    if (overflow !== "auto" && overflow !== "scroll") continue;
    if (el.scrollLeft > 0) left = true;
    if (el.scrollLeft + el.clientWidth < el.scrollWidth - 1) right = true;
    if (left && right) break;
  }
  return { left, right };
}
