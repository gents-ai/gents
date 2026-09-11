/* Native host adapter: screens keep calling href/navigate/useRoute.
   MemoryNavProvider owns the stack; this module never writes location. */
import { useNav, type Nav, type Route, pathFor } from "@gents/shell";

export type { Route };
export { pathFor };

let current: Nav | null = null;

export function bindNav(nav: Nav) {
  current = nav;
}

export function href(route: Route) {
  return current?.href(route) ?? "#";
}

export function navigate(route: Route) {
  current?.navigate(route);
}

export function useRoute(): Route {
  return useNav().route;
}
