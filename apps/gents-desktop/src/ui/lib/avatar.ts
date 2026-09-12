/* one of the drawn avatars in public/avatars, picked by the agent's name
   so an agent keeps the same picture everywhere */
const AVATARS = 7;
export function avatarFor(name: string) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `/avatars/${(h % AVATARS) + 1}.png`;
}
