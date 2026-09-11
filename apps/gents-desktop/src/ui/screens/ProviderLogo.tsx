/* a provider's mark at icon size, inverted for dark so a black-on-clear
   SVG reads on either ground; a neutral server glyph when none applies */
import { Server } from 'lucide-react'
import { cn } from '@gents/ui/lib/utils'
import { providerLogo } from '@/lib/provider'

export function ProviderLogo({
  kind,
  endpoint,
  className,
}: {
  kind: string | null | undefined
  endpoint?: string | null
  className?: string
}) {
  const src = providerLogo(kind, endpoint)
  return src ? (
    <img src={src} alt="" className={cn('size-4 shrink-0 object-contain dark:invert', className)} />
  ) : (
    <Server className={cn('size-4 shrink-0 text-muted-foreground', className)} />
  )
}
