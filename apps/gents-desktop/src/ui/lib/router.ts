/* Native host adapter: screens keep calling href/navigate/useRoute.
   MemoryNavProvider owns the stack; this module never writes location.
   href() is a path token (not a URL). Clicks on those tokens, and leftover
   prototype `#/…` hrefs, are intercepted and pushed onto the memory stack. */
import { parseRoute, pathFor, useNav, type Nav, type Route } from "@gents/shell";

export type { Route };
export { pathFor, parseRoute };

let current: Nav | null = null;

export function bindNav(nav: Nav) {
  current = nav;
}

export function href(route: Route) {
  return pathFor(route);
}

export function navigate(route: Route) {
  current?.navigate(route);
}

export function useRoute(): Route {
  return useNav().route;
}

export function interceptNavClicks(root: ParentNode = document): () => void {
  const onClick = (event: Event) => {
    const mouse = event as MouseEvent;
    if (
      mouse.button !== 0 ||
      mouse.metaKey ||
      mouse.ctrlKey ||
      mouse.shiftKey ||
      mouse.altKey
    ) {
      return;
    }
    const target = mouse.target;
    if (!(target instanceof Element)) return;
    const a = target.closest("a");
    if (!(a instanceof HTMLAnchorElement)) return;
    const raw = a.getAttribute("href");
    if (!raw || raw === "#") return;
    if (/^(https?:|mailto:|tel:)/i.test(raw)) return;
    if (!(raw.startsWith("/") || raw.startsWith("#/"))) return;
    mouse.preventDefault();
    navigate(parseRoute(raw));
  };
  root.addEventListener("click", onClick, true);
  return () => root.removeEventListener("click", onClick, true);
}
