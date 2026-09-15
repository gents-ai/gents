/* one of the drawn avatars in public/avatars, picked by the agent's name
   so an agent keeps the same picture everywhere. The pick depends on the
   count, so changing AVATARS re-picks most agents' pictures. The files are
   128px and compressed: the largest avatar shows at 36px, which stays sharp
   on a 3x screen. */
const AVATARS = 16;
export function avatarFor(name: string) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `/avatars/${(h % AVATARS) + 1}.png`;
}
