import { Toaster } from "@gents/ui/components/sonner";
import { usePreferences } from "@/preferences";

/** The app's notices, drawn in the theme the person chose: the kit's toaster
    otherwise draws light, and its second line is unreadable on dark. */
export function AppToaster() {
  const theme = usePreferences((p) => p.theme);
  return <Toaster closeButton theme={theme} />;
}
