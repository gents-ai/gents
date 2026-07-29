import { CopyButton } from "@source-inc/gents-desktop-ui";

/// The one global failure surface: shell-level errors (bridge, snapshot,
/// action fallbacks) land here. Editor-local failures render next to their
/// forms; this banner is for everything without a nearer home.
export function ErrorBanner({
  message,
  onDismiss,
}: {
  message: string;
  onDismiss: () => void;
}) {
  return (
    <div className="callout error-banner" data-testid="error-banner" role="alert">
      <svg aria-hidden="true" className="error-banner-icon" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7.5v5.5" />
        <path d="M12 16.4h.01" />
      </svg>
      <p className="error-banner-message">{message}</p>
      <div className="error-banner-actions">
        <CopyButton label="Copy error" getText={() => message} />
        <button
          aria-label="Dismiss error"
          className="ghost-button error-banner-dismiss"
          data-testid="error-banner-dismiss"
          onClick={onDismiss}
          type="button"
        >
          <svg aria-hidden="true" viewBox="0 0 24 24">
            <path d="m6 6 12 12" />
            <path d="m18 6-12 12" />
          </svg>
        </button>
      </div>
    </div>
  );
}
