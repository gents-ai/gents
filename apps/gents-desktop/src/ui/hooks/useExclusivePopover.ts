import { useCallback, useEffect, useId, useRef, useState } from "react";

type PopoverController = {
  activate: () => void;
  closeForPeer: () => boolean;
};

let activePopoverId: string | null = null;
let pendingPopoverId: string | null = null;
const controllers = new Map<string, PopoverController>();

function activatePending() {
  const pending = pendingPopoverId;
  pendingPopoverId = null;
  if (!pending) return;
  const controller = controllers.get(pending);
  if (!controller) return;
  activePopoverId = pending;
  controller.activate();
}

function release(id: string) {
  if (activePopoverId !== id) return;
  activePopoverId = null;
  activatePending();
}

/** Keep the shell to one mounted popover across Base UI exit animations. */
export function useExclusivePopover(onClose?: () => void) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const requestedOpenRef = useRef(false);
  // Only detects never-mounted cancellation; Base UI acknowledges mounted closes.
  const popupRef = useRef<HTMLDivElement | null>(null);
  const onCloseRef = useRef(onClose);

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const controller: PopoverController = {
      activate: () => {
        requestedOpenRef.current = true;
        setOpen(true);
      },
      closeForPeer: () => {
        if (!requestedOpenRef.current) return popupRef.current !== null;
        requestedOpenRef.current = false;
        const awaitsUnmount = popupRef.current !== null;
        setOpen(false);
        onCloseRef.current?.();
        return awaitsUnmount;
      },
    };
    controllers.set(id, controller);
    return () => {
      controllers.delete(id);
      if (pendingPopoverId === id) pendingPopoverId = null;
      if (activePopoverId === id) release(id);
    };
  }, [id]);

  const onOpenChange = useCallback(
    (next: boolean) => {
      const controller = controllers.get(id);
      if (!controller) return;
      if (!next) {
        const wasRequestedOpen = requestedOpenRef.current;
        requestedOpenRef.current = false;
        if (pendingPopoverId === id) pendingPopoverId = null;
        setOpen(false);
        if (wasRequestedOpen) onCloseRef.current?.();
        if (activePopoverId === id && popupRef.current === null) release(id);
        return;
      }
      requestedOpenRef.current = true;
      if (activePopoverId === id) {
        pendingPopoverId = null;
        setOpen(true);
        return;
      }
      if (activePopoverId === null) {
        activePopoverId = id;
        controller.activate();
        return;
      }
      pendingPopoverId = id;
      const closingId = activePopoverId;
      const active = controllers.get(closingId);
      if (!active || !active.closeForPeer()) release(closingId);
    },
    [id],
  );

  const onOpenChangeComplete = useCallback(
    (next: boolean) => {
      if (!next && !requestedOpenRef.current) release(id);
    },
    [id],
  );

  return { open, onOpenChange, onOpenChangeComplete, popupRef };
}
