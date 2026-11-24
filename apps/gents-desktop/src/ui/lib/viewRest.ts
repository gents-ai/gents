/* when the transcript's view rests: no finger or scrollbar drag holds it,
   no motion has come for REST_MS, and it has stood still across
   STILL_FRAMES drawn frames. WebKit moves an animated wheel scroll once a
   frame, and a loaded page draws frames further apart than REST_MS, so
   time alone calls the view rested between two steps of one wheel, and a
   position written then cuts the wheel's travel short */
export const REST_MS = 150;
export const STILL_FRAMES = 2;

export type ViewMotion = {
  touching: boolean;
  dragging: boolean;
  sinceMotionMs: number;
  stillFrames: number;
};

export const viewRests = (motion: ViewMotion) =>
  !motion.touching &&
  !motion.dragging &&
  motion.sinceMotionMs >= REST_MS &&
  motion.stillFrames >= STILL_FRAMES;
