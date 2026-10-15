/* The box a plugin's page shows through. Mounting it asks the plugin's
   frame to draw the surface and places the frame over this element;
   unmounting hides the surface and the frame. The shell's re-renders
   reach the page as new props, never as a remount. */
import { useEffect, useRef } from "react";

import type { SandboxHost } from "../sandbox-host";

export function FrameViewport({
  host,
  surface,
  props,
}: {
  host: SandboxHost;
  surface: string;
  props: Record<string, unknown>;
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
      className="h-full min-h-[70vh] w-full"
    />
  );
}
