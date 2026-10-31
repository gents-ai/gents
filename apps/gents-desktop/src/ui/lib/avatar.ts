/** The node portraits, public/avatars/1.png to 89.png; a node's avatar names
    one by its number. */
export const AVATARS = 89;

const hash = (seed: string) => {
  let h = 0;
  for (const c of seed) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h;
};

const known = (key: string | null | undefined): key is string => {
  if (!key || !/^\d+$/.test(key)) return false;
  const n = Number(key);
  return n >= 1 && n <= AVATARS;
};

/** The portrait a node without a chosen one is drawn with, steady for the
    same name. */
export const avatarKey = (seed: string) => String((hash(seed) % AVATARS) + 1);

/** The portrait to draw: the chosen one, or else the name's. */
export const shownAvatar = (avatar: string | null | undefined, seed: string) =>
  known(avatar) ? avatar : avatarKey(seed);

export const avatarSrc = (key: string) => `/avatars/${key}.png`;

export function avatarFor(name: string) {
  return avatarSrc(avatarKey(name));
}

/** A random portrait other than `current`. */
export function randomAvatar(current: string | null | undefined): string {
  const pick = () => String(Math.floor(Math.random() * AVATARS) + 1);
  let next = pick();
  while (next === current) next = pick();
  return next;
}
