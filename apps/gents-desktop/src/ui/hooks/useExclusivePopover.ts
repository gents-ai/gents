import { useCallback, useEffect, useId, useRef, useState } from "react";

const POPOVER_OPEN_EVENT = "gents:popover-open";
const POPOVER_EXIT_MS = 150;
let activePopoverId: string | null = null;

/** Keep the shell to one open popover even when Base UI portals are siblings. */
export function useExclusivePopover(onClose?: () => void) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const onCloseRef = useRef(onClose);
  const openTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelPendingOpen = useCallback(() => {
    if (openTimerRef.current !== null) {
      clearTimeout(openTimerRef.current);
      openTimerRef.current = null;
    }
  }, []);

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const closeForPeer = (event: Event) => {
      if ((event as CustomEvent<string>).detail === id) return;
      cancelPendingOpen();
      setOpen(false);
      onCloseRef.current?.();
    };
    document.addEventListener(POPOVER_OPEN_EVENT, closeForPeer);
    return () => {
      document.removeEventListener(POPOVER_OPEN_EVENT, closeForPeer);
      cancelPendingOpen();
      if (activePopoverId === id) activePopoverId = null;
    };
  }, [cancelPendingOpen, id]);

  const onOpenChange = useCallback(
    (next: boolean) => {
      if (next) {
        const replacingPeer = activePopoverId !== null && activePopoverId !== id;
        activePopoverId = id;
        document.dispatchEvent(new CustomEvent(POPOVER_OPEN_EVENT, { detail: id }));
        cancelPendingOpen();
        if (replacingPeer) {
          // Base UI retains a closing popup for its 100 ms exit animation.
          // Wait past that boundary before mounting the next dialog-role
          // popup, so accessibility clients never observe stacked dialogs.
          setOpen(false);
          openTimerRef.current = setTimeout(() => {
            openTimerRef.current = null;
            if (activePopoverId === id) setOpen(true);
          }, POPOVER_EXIT_MS);
        } else {
          setOpen(true);
        }
      } else {
        cancelPendingOpen();
        if (activePopoverId === id) activePopoverId = null;
        setOpen(false);
        onCloseRef.current?.();
      }
    },
    [cancelPendingOpen, id],
  );

  return { open, onOpenChange };
}
