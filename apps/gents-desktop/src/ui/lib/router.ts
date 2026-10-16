// Native host adapter: screens keep calling href/navigate/useRoute;
// MemoryNavProvider owns the stack and this module never writes location.
import { parseRoute, pathFor, useNav, type Nav, type Route } from "@gents/shell";

export type { Route };
export { pathFor, parseRoute };

let current: Nav | null = null;

export function bindNav(nav: Nav | null) {
  current = nav;
}

export function href(route: Route) {
  return pathFor(route);
}

export function navigate(route: Route) {
  if (!current || pathFor(route) === pathFor(current.route)) return;
  current.navigate(route);
}

/* a draft's save can settle after the person has left it, and must not
   take them back */
const navShowing = (route: Route) =>
  current && pathFor(route) === pathFor(current.route) ? current : null;

export function leave(from: Route, fallback: Route) {
  const nav = navShowing(from);
  if (!nav) return;
  if (nav.canBack) nav.back();
  else navigate(fallback);
}

/* the memory nav has no replace: going back one entry and pushing drops
   the draft from the history */
export function replace(from: Route, to: Route) {
  const nav = navShowing(from);
  if (!nav) return;
  if (nav.canBack) nav.back();
  nav.navigate(to);
}

export function useRoute(): Route {
  return useNav().route;
}

export type History = Pick<Nav, "canBack" | "canForward" | "back" | "forward">;

export function useHistory(): History {
  const nav = useNav();
  return {
    canBack: nav.canBack,
    canForward: nav.canForward,
    back: nav.back,
    forward: nav.forward,
  };
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
