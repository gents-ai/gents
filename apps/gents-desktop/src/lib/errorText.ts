/** What an error says, for a person: its message, or the value itself
    when a bridge call rejected with something other than an Error. */
export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
