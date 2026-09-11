/* The pieces every configuration editor shares: a draft that saves as
   it changes (choices at once, text when the field is left) with a toast,
   and the row types the desktop app's panels use: text, number, lines,
   choice, switch, textarea. Validation follows the desktop app's hints. */
import type { ReactNode } from 'react'
import { Input } from '@gents/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@gents/ui/components/select'
import { Switch } from '@gents/ui/components/switch'
import { Textarea } from '@gents/ui/components/textarea'
import { Fact, Row } from './rows'

export type Choice = { value: string; label: string }

type Common = { id: string; label: ReactNode; description?: ReactNode }

export function TextRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  onEnter,
  placeholder,
  mono,
  password,
  wide,
}: Common & {
  value: string
  onChange: (v: string) => void
  onCommit: () => void
  onEnter: (e: React.KeyboardEvent<HTMLElement>) => void
  placeholder?: string
  mono?: boolean
  password?: boolean
  wide?: boolean
}) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Input
        id={id}
        type={password ? 'password' : 'text'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        onKeyDown={onEnter}
        placeholder={placeholder}
        className={`${wide ? 'w-96 max-md:w-full' : 'w-72 max-md:w-full'} ${mono ? 'font-mono text-xs' : ''}`}
      />
    </Row>
  )
}

/* a number kept as text while editing; empty means null */
export function NumberRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  onEnter,
  placeholder,
}: Common & {
  value: string
  onChange: (v: string) => void
  onCommit: () => void
  onEnter: (e: React.KeyboardEvent<HTMLElement>) => void
  placeholder?: string
}) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Input
        id={id}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        onKeyDown={onEnter}
        placeholder={placeholder}
        className="w-36"
      />
    </Row>
  )
}

export function AreaRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  placeholder,
  rows = 3,
  mono,
}: Common & {
  value: string
  onChange: (v: string) => void
  onCommit: () => void
  placeholder?: string
  rows?: number
  mono?: boolean
}) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Textarea
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        placeholder={placeholder}
        rows={rows}
        className={`w-96 max-md:w-full ${mono ? 'font-mono text-xs' : ''}`}
      />
    </Row>
  )
}

export function ChoiceRow({
  id,
  label,
  description,
  value,
  onChange,
  items,
  none,
}: Common & { value: string; onChange: (v: string) => void; items: Choice[]; none?: string }) {
  const all = none ? [{ value: '', label: none }, ...items] : items
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Select items={all} value={value} onValueChange={(v) => onChange(v ?? '')}>
        <SelectTrigger id={id} className="w-72 max-md:w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {all.map((i) => (
            <SelectItem key={i.value || '∅'} value={i.value}>
              {i.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </Row>
  )
}

export function SwitchRow({
  id,
  label,
  description,
  checked,
  onChange,
}: Common & { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Switch id={id} checked={checked} onCheckedChange={onChange} />
    </Row>
  )
}

export function FactRow({
  label,
  description,
  children,
  mono,
}: Omit<Common, 'id'> & { children: ReactNode; mono?: boolean }) {
  return (
    <Row label={label} description={description}>
      <Fact mono={mono}>{children}</Fact>
    </Row>
  )
}
