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
  const releaseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelPendingOpen = useCallback(() => {
    if (openTimerRef.current !== null) {
      clearTimeout(openTimerRef.current);
      openTimerRef.current = null;
    }
  }, []);

  const cancelPendingRelease = useCallback(() => {
    if (releaseTimerRef.current !== null) {
      clearTimeout(releaseTimerRef.current);
      releaseTimerRef.current = null;
    }
  }, []);

  const releaseAfterExit = useCallback(() => {
    cancelPendingRelease();
    releaseTimerRef.current = setTimeout(() => {
      releaseTimerRef.current = null;
      if (activePopoverId === id) activePopoverId = null;
    }, POPOVER_EXIT_MS);
  }, [cancelPendingRelease, id]);

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
      cancelPendingRelease();
      // Unmount removes this hook's portal rather than running its exit state.
      if (activePopoverId === id) activePopoverId = null;
    };
  }, [cancelPendingOpen, cancelPendingRelease, id]);

  const onOpenChange = useCallback(
    (next: boolean) => {
      if (next) {
        cancelPendingRelease();
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
        if (activePopoverId === id) releaseAfterExit();
        setOpen(false);
        onCloseRef.current?.();
      }
    },
    [cancelPendingOpen, cancelPendingRelease, id, releaseAfterExit],
  );

  return { open, onOpenChange };
}
