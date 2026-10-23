// Native host adapter: screens keep calling href/navigate/useRoute;
// MemoryNavProvider owns the stack and this module never writes location.
import {
  parseRoute as shellParseRoute,
  pathFor as shellPathFor,
  useNav,
  type Nav,
  type Route as ShellRoute,
} from "@gents/shell";

/* the desktop's routes: the kit's, and a UX plugin's own page. The kit's
   memory history carries any route object and never re-parses, so the
   extra route costs it nothing; only the path functions must know it. */
export type Route =
  | ShellRoute
  | { name: "plugin"; pluginId: string }
  /* the sessions one session started, a level down the sessions list */
  | { name: "sessionsUnder"; session: string };

export function pathFor(route: Route): string {
  if (route.name === "plugin") return `/plugins/${encodeURIComponent(route.pluginId)}`;
  if (route.name === "sessionsUnder") {
    return `/sessions/under/${encodeURIComponent(route.session)}`;
  }
  return shellPathFor(route);
}

export function parseRoute(path: string): Route {
  const parts = path.replace(/^#\/?/, "").replace(/^\//, "").split("/").filter(Boolean);
  if (parts[0] === "plugins" && parts[1]) {
    return { name: "plugin", pluginId: decodeURIComponent(parts[1]) };
  }
  if (parts[0] === "sessions" && parts[1] === "under" && parts[2]) {
    return { name: "sessionsUnder", session: decodeURIComponent(parts[2]) };
  }
  return shellParseRoute(path);
}

/* the kit's nav typed with the desktop's routes */
type DesktopNav = Omit<Nav, "route" | "navigate"> & {
  readonly route: Route;
  navigate: (route: Route) => void;
};

const asDesktop = (nav: Nav): DesktopNav => nav as unknown as DesktopNav;

let current: DesktopNav | null = null;

export function bindNav(nav: Nav | null) {
  current = nav ? asDesktop(nav) : null;
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
  return asDesktop(useNav()).route;
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
