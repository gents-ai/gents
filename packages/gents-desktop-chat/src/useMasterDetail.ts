import { useCallback, useState } from "react";
import { NARROW_BREAKPOINT_PX } from "@source-inc/gents-desktop-client";

/**
 * Headless master/detail pane switching from the iPhone branch.
 * Hosts own layout; this only tracks whether the detail pane should show.
 */
export function useMasterDetail(options?: { breakpointPx?: number }) {
  const breakpoint = options?.breakpointPx ?? NARROW_BREAKPOINT_PX;
  const [detailOpen, setDetailOpen] = useState(false);

  const openDetail = useCallback(() => setDetailOpen(true), []);
  const closeDetail = useCallback(() => setDetailOpen(false), []);

  return {
    breakpointPx: breakpoint,
    detailOpen,
    openDetail,
    closeDetail,
    setDetailOpen,
  };
}
