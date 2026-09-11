import { cn } from '@gents/ui/lib/utils'
import { initials } from './behavior'
import { avatarFor } from '@/lib/avatar'

/* the agent's picture: one of the drawn avatars in public/avatars, picked
   by the agent's name so it stays the same everywhere; initials only
   when a picture is refused with src={null} */
export function AgentAvatar({
  name,
  src,
  className,
}: {
  name: string
  src?: string | null
  className?: string
}) {
  const picture = src === undefined ? avatarFor(name) : src
  return picture ? (
    <img src={picture} alt="" className={cn('size-8 shrink-0 rounded-full', className)} />
  ) : (
    <span
      className={cn(
        'grid size-8 shrink-0 place-items-center rounded-full bg-muted font-heading text-sm text-heading',
        className,
      )}
      aria-hidden="true"
    >
      {initials(name)}
    </span>
  )
}
