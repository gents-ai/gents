/* The OS folder picker, through Tauri's dialog plugin when the app runs in
   the desktop shell. In a plain browser (this prototype on the web) there
   is none, so the caller falls back to the typed path. */
export async function pickDirectory(options: {
  defaultPath?: string | null;
  title?: string;
}): Promise<string | null> {
  if (!("__TAURI_INTERNALS__" in window)) return null;
  /* a variable specifier so Vite and TypeScript leave the desktop-only module alone */
  const plugin = "@tauri-apps/plugin-dialog";
  const { open } = (await import(/* @vite-ignore */ plugin)) as {
    open: (o: {
      directory: boolean;
      multiple: boolean;
      defaultPath?: string;
      title?: string;
    }) => Promise<string | string[] | null>;
  };
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
