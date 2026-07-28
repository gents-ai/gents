import { tauriTransport, type ClientUpdateEvent } from "./transport.js";

export type DesktopClientUpdatedEvent = ClientUpdateEvent;
export type DesktopClientUpdatedHandler = (
  event: DesktopClientUpdatedEvent,
) => void | Promise<void>;
export type DesktopClientUpdatedUnlisten = () => void;
export type DesktopClientUpdatedListenerFactory = (
  handler: DesktopClientUpdatedHandler,
) => Promise<DesktopClientUpdatedUnlisten>;
export type DesktopClientUpdatedErrorHandler = (error: unknown) => void;

async function defaultDesktopClientUpdatedListenerFactory(
  handler: DesktopClientUpdatedHandler,
) {
  return tauriTransport().listenClientUpdated(handler);
}

let desktopClientUpdatedListenerFactoryOverride: DesktopClientUpdatedListenerFactory | null =
  null;

export function setDesktopClientUpdatedListenerFactoryForTests(
  factory: DesktopClientUpdatedListenerFactory | null,
) {
  desktopClientUpdatedListenerFactoryOverride = factory;
}

export function listenToDesktopClientUpdates(
  handler: DesktopClientUpdatedHandler,
  onError: DesktopClientUpdatedErrorHandler = () => undefined,
  listenerFactory?: DesktopClientUpdatedListenerFactory,
) {
  const safeHandler: DesktopClientUpdatedHandler = (event) =>
    Promise.resolve(handler(event)).catch((error) => {
      try {
        onError(error);
      } catch {
        // Error reporting must not create another unhandled event rejection.
      }
    });
  return (
    listenerFactory ??
    desktopClientUpdatedListenerFactoryOverride ??
    defaultDesktopClientUpdatedListenerFactory
  )(safeHandler);
}
