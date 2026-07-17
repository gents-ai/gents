/// Long document/request ids are unreadable at drawer width — display a
/// stable prefix and keep the full id in the title (and for copying).
export function shortId(id: string, max = 14): string {
  return id.length > max ? `${id.slice(0, max)}…` : id;
}
