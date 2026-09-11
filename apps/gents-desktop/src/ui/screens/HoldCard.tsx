/* Something that waits on a person: a raised card with a title, a line of
   detail and the actions. Used for held tool calls in a session and for
   items in the mailbox. */
import type { ReactNode } from 'react'
import { Button } from '@gents/ui/components/button'

export function HoldCard({
  title,
  detail,
  onApprove,
  onDeny,
  children,
}: {
  title: ReactNode
  detail?: ReactNode
  onApprove?: () => void
  onDeny?: () => void
  children?: ReactNode
}) {
  return (
    <div className="rounded-2xl border border-border/60 bg-raised px-4 py-3">
      <p className="text-sm font-medium">{title}</p>
      {detail && <div className="mt-1 text-sm text-muted-foreground">{detail}</div>}
      <div className="mt-3 flex items-center gap-2">
        {onApprove && (
          <Button size="sm" variant="brand" onClick={onApprove}>
            Approve
          </Button>
        )}
        {onDeny && (
          <Button size="sm" variant="outline" onClick={onDeny}>
            Deny
          </Button>
        )}
        {children}
      </div>
    </div>
  )
}
