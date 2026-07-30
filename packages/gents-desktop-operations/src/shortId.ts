export function shortId(id: string, max = 14): string {
  return id.length > max ? `${id.slice(0, max)}…` : id;
}
