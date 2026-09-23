/* The OS folder picker, through Tauri's dialog plugin when the app runs in
   the desktop shell. In a plain browser there is none, so the caller falls
   back to the typed path.

   The import is a literal so Vite bundles the plugin: the packaged webview
   cannot resolve a bare module specifier at runtime. */
export async function pickDirectory(options: {
  defaultPath?: string | null;
  title?: string;
}): Promise<string | null> {
  if (!canPickDirectory()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    defaultPath: options.defaultPath ?? undefined,
    title: options.title,
  });
  return typeof picked === "string" ? picked : null;
}

/* whether a picker exists here at all: the button is shown only then */
export const canPickDirectory = () => "__TAURI_INTERNALS__" in window;
