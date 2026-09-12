import { useCallback, useEffect, useId, useRef, useState } from "react";

const POPOVER_OPEN_EVENT = "gents:popover-open";

/** Keep the shell to one open popover even when Base UI portals are siblings. */
export function useExclusivePopover(onClose?: () => void) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const onCloseRef = useRef(onClose);

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const closeForPeer = (event: Event) => {
      if ((event as CustomEvent<string>).detail === id) return;
      setOpen(false);
      onCloseRef.current?.();
    };
    document.addEventListener(POPOVER_OPEN_EVENT, closeForPeer);
    return () => document.removeEventListener(POPOVER_OPEN_EVENT, closeForPeer);
  }, [id]);

  const onOpenChange = useCallback(
    (next: boolean) => {
      setOpen(next);
      if (next) {
        document.dispatchEvent(new CustomEvent(POPOVER_OPEN_EVENT, { detail: id }));
      } else {
        onCloseRef.current?.();
      }
    },
    [id],
  );

  return { open, onOpenChange };
}
