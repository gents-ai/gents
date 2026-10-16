const AVATARS = 16;
export function avatarFor(name: string) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `/avatars/${(h % AVATARS) + 1}.png`;
}
