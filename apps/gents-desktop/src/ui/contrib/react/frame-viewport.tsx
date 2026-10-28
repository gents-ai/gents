/* The box a plugin's page shows through. Mounting it asks the plugin's
   frame to draw the surface and places the frame over this element;
   unmounting hides the surface and the frame. The shell's re-renders
   reach the page as new props, never as a remount. A page `fill`s its
   container (the plugin route's whole pane); an agent section keeps a
   tall minimum inside the agent screen's scrolling column. */
import { useEffect, useRef } from "react";

import type { SandboxHost } from "../sandbox-host";

export function FrameViewport({
  host,
  surface,
  props,
  fill = false,
}: {
  host: SandboxHost;
  surface: string;
  props: Record<string, unknown>;
  fill?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const propsKey = JSON.stringify(props);
  useEffect(() => {
    host.show(surface, JSON.parse(propsKey) as Record<string, unknown>);
  }, [host, surface, propsKey]);
  useEffect(() => {
    host.attach(surface, ref.current);
    return () => {
      host.attach(surface, null);
      host.hide(surface);
    };
  }, [host, surface]);
  return (
    <div
      ref={ref}
      data-testid="ux-frame-viewport"
      data-surface={surface}
      data-fill={fill ? "" : undefined}
      className={fill ? "h-full min-h-0 w-full flex-1" : "h-full min-h-[70vh] w-full"}
    />
  );
}
