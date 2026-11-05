// Native host adapter: screens keep calling href/navigate/useRoute;
// MemoryNavProvider owns the stack and this module never writes location.
import {
  parseRoute as shellParseRoute,
  pathFor as shellPathFor,
  useNav,
  type Nav,
  type Route as ShellRoute,
} from "@gents/shell";

import { flag } from "@/preferences";
import { SHARING_SECTION, startJoin } from "./joinInvite";
import { isAppLink, linkRoute, parseLink } from "./links";

/* the desktop's routes: the kit's, and a UX plugin's own page. The kit's
   memory history carries any route object and never re-parses, so the
   extra route costs it nothing; only the path functions must know it. */
export type Route =
  | ShellRoute
  | { name: "plugin"; pluginId: string }
  /* the sessions one session started, a level down the sessions list */
  | { name: "sessionsUnder"; session: string }
  /* the sessions archived out of the list */
  | { name: "sessionsArchived" }
  /* the active node's packs; `item` opens one pack, or `local:<id>` a plugin
     loaded from the home folder; `review` a pack previewed from a file or a
     link, by its preview key; `doc` one document it would add, as
     `<collection>:<id>` */
  | { name: "packs"; item?: string; review?: string; doc?: string };

export function pathFor(route: Route): string {
  if (route.name === "plugin") return `/plugins/${encodeURIComponent(route.pluginId)}`;
  if (route.name === "sessionsArchived") return "/sessions/archived";
  if (route.name === "packs") {
    const page = route.review
      ? `/packs/review/${encodeURIComponent(route.review)}`
      : route.item
        ? `/packs/${encodeURIComponent(route.item)}`
        : "/packs";
    return route.doc && page !== "/packs"
      ? `${page}/${encodeURIComponent(route.doc)}`
      : page;
  }
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
  if (parts[0] === "packs") {
    /* a pack's name is one encoded segment, so `review` can't be one */
    const [page, doc] =
      parts[1] === "review" && parts[2]
        ? [{ review: decodeURIComponent(parts[2]) }, parts[3]]
        : parts[1]
          ? [{ item: decodeURIComponent(parts[1]) }, parts[2]]
          : [{}, undefined];
    return { name: "packs", ...page, ...(doc ? { doc: decodeURIComponent(doc) } : {}) };
  }
  if (parts[0] === "sessions" && parts[1] === "archived") {
    return { name: "sessionsArchived" };
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

/* how often the route has changed since launch, so work begun at launch
   can tell the person has since moved on */
let moves = 0;
let lastPath: string | null = null;

export function bindNav(nav: Nav | null) {
  current = nav ? asDesktop(nav) : null;
  if (!current) return;
  const path = pathFor(current.route);
  if (lastPath !== null && path !== lastPath) moves += 1;
  lastPath = path;
}

/** How many times the route has changed since launch. */
export const routeMoves = () => moves;

export function href(route: Route) {
  return pathFor(route);
}

type NavigationGuard = (
  route: Route,
  proceed: () => void,
  cancel?: () => void,
) => boolean;
let navigationGuard: NavigationGuard | null = null;

export function bindNavigationGuard(guard: NavigationGuard): () => void {
  navigationGuard = guard;
  return () => {
    if (navigationGuard === guard) navigationGuard = null;
  };
}

export function reviewNavigation(
  route: Route,
  proceed: () => void,
  cancel?: () => void,
): boolean {
  if (navigationGuard && !navigationGuard(route, proceed, cancel)) return false;
  proceed();
  return true;
}

function navigateReviewed(route: Route): boolean {
  if (!current || pathFor(route) === pathFor(current.route)) return true;
  return reviewNavigation(route, () => current?.navigate(route));
}

export function navigate(route: Route) {
  navigateReviewed(route);
}

/* the node this desktop runs, once known: an invite opens there */
let homeNode: string | null = null;

export function bindHomeNode(nodeDid: string | null) {
  homeNode = nodeDid;
}

/** Follows a `gents:` link, from a message or from outside the app. A link
    only goes somewhere: it never installs, joins or sends. A pack link opens
    that pack's page with what it would add, which installs only when the
    person chooses Install there and again in its review; an invite link opens
    the Sharing section of this desktop's node with the join dialog on the
    invite's preview, which joins only when the person chooses Join. False when
    the link names nothing this app opens, or the person stayed where they
    were. */
export function openLink(href: string): boolean {
  const link = parseLink(href);
  if (
    !link ||
    (link.kind === "pack" && !flag("packs")) ||
    (link.kind === "invite" && (!flag("sharing") || !homeNode))
  ) {
    console.warn("ignored a gents link this app doesn't open", href);
    return false;
  }
  if (link.kind === "invite") {
    const route: Route = {
      name: "agent",
      nodeDid: homeNode!,
      section: SHARING_SECTION,
    };
    return reviewNavigation(route, () => {
      if (current && pathFor(route) !== pathFor(current.route)) current.navigate(route);
      startJoin(link.invite);
    });
  }
  return navigateReviewed(linkRoute(link));
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

/** The route showing now, for an action begun outside the page's tree; null
    before the nav is bound. */
export function currentRoute(): Route | null {
  return current?.route ?? null;
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
    if (isAppLink(raw)) {
      mouse.preventDefault();
      if (!openLink(raw)) mouse.stopImmediatePropagation();
      return;
    }
    if (!(raw.startsWith("/") || raw.startsWith("#/"))) return;
    mouse.preventDefault();
    if (!navigateReviewed(parseRoute(raw))) mouse.stopImmediatePropagation();
  };
  root.addEventListener("click", onClick, true);
  return () => root.removeEventListener("click", onClick, true);
}
