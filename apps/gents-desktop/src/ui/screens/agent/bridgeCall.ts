/* One bridge command from a panel that has no snapshot of its own. */
export async function call<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { bridgeCommand } = await import("@source-inc/gents-desktop-client");
  return (await invoke(bridgeCommand(command as never), args)) as T;
}

export function message(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
