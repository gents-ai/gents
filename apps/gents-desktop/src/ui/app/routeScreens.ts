/* What each route draws in the pane, by route name: an extension point, so
   a screen is one entry and the app body does not change. */
import type { ReactNode } from "react";
import type { Route } from "@/lib/router";
import { createRegistry } from "./registry";

export type RouteScreen = { id: string; render: (route: Route) => ReactNode };

export const routeScreens = createRegistry<RouteScreen>();

/** One screen per route the router knows, each handed its own kind of route. */
export type ScreensByRoute = {
  [N in Route["name"]]: (route: Extract<Route, { name: N }>) => ReactNode;
};

/** Registers a screen for every route the router knows: the type refuses a
    route left without one. Returns what removes them again. */
export function registerRouteScreens(screens: ScreensByRoute): () => void {
  const removals = Object.entries(screens).map(([id, render]) =>
    /* each entry is called only with a route of its own name */
    routeScreens.register({ id, render: render as (route: Route) => ReactNode }),
  );
  return () => removals.forEach((remove) => remove());
}

/** The screen registered for `route`, or nothing while none is. */
export function RouteScreenOutlet({ route }: { route: Route }) {
  const screen = routeScreens.useItem(route.name);
  return screen ? screen.render(route) : null;
}
