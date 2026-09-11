/* the settings-group pattern from the kit, with the pieces this screen repeats */
import type { ReactNode } from 'react'
import {
  FieldContent,
  FieldDescription,
  FieldLabel,
  FieldLegend,
  FieldRow,
  FieldRows,
  FieldSet,
} from '@gents/ui/components/field'

export function Group({
  title,
  children,
  action,
}: {
  title: string
  children: ReactNode
  action?: ReactNode
}) {
  return (
    <FieldSet className="mb-8">
      <div className="flex items-baseline justify-between">
        <FieldLegend variant="eyebrow">{title}</FieldLegend>
        {action}
      </div>
      <FieldRows>{children}</FieldRows>
    </FieldSet>
  )
}

export function Row({
  label,
  description,
  htmlFor,
  children,
}: {
  label: ReactNode
  description?: ReactNode
  htmlFor?: string
  children?: ReactNode
}) {
  return (
    <FieldRow className="max-md:flex-col max-md:items-stretch max-md:gap-3">
      <FieldContent>
        <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel>
        {description && <FieldDescription>{description}</FieldDescription>}
      </FieldContent>
      {children}
    </FieldRow>
  )
}

/* a read-only value, in mono when it is an identifier */
export function Fact({ children, mono = false }: { children: ReactNode; mono?: boolean }) {
  return (
    <span
      className={`max-w-[28rem] truncate text-sm text-muted-foreground max-md:max-w-full max-md:break-all max-md:whitespace-normal ${mono ? 'font-mono text-xs' : ''}`}
    >
      {children ?? '—'}
    </span>
  )
}
