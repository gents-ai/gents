/* The App Shell from the Branding file: a header with the mark, a
   breadcrumb and settings; a rail with the agent, new session, mailbox and
   sessions; the content slot on the ground. Chrome and canvas share the
   ground; content that needs a surface brings its own. */
import type { ReactElement, ReactNode } from 'react'
import { CircleAlert, Inbox, Menu, Plus, ScrollText, SlidersHorizontal, X } from 'lucide-react'
import { useState } from 'react'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@gents/ui/components/dropdown-menu'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '@gents/ui/components/breadcrumb'
import { Button } from '@gents/ui/components/button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@gents/ui/components/tooltip'
import { cn } from '@gents/ui/lib/utils'
import { href, type Route } from '@/lib/router'
import { applyTheme, themePreference, type ThemePreference } from '@/theme'
import { navPreference, saveNavPreference, type NavMode } from '@/nav'
import { useMediaQuery } from '@/lib/media'
import { AgentAvatar } from '@/screens/AgentAvatar'
import { AgentHoverCard } from '@/screens/HoverCards'
import type { DeploymentView, SyncHealthView } from '@source-inc/gents-desktop-client'
import { Mark } from './Mark'
import { SyncHealth } from './SyncHealth'
import { NavPanel, RailFlyout } from './RailFlyout'
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from '@gents/ui/components/sheet'

function RailItem({
  label,
  active,
  count,
  to,
  children,
}: {
  label: string
  active?: boolean
  count?: number
  to: Route
  children: ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <a
            href={href(to)}
            aria-label={label}
            aria-current={active ? 'page' : undefined}
            className={cn(
              'relative grid size-8 place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground',
              active && 'border-border/60 bg-raised text-ink shadow-xs',
            )}
          />
        }
      >
        {children}
        {count ? (
          <span className="absolute -top-1.5 -right-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
            {count}
          </span>
        ) : null}
      </TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  )
}

/* global settings live behind the sliders: for now, the theme */
function SettingsMenu({
  nav,
  onNav,
  showNav,
  trigger,
  children,
}: {
  nav: NavMode
  onNav: (mode: NavMode) => void
  /** the side nav choices only make sense where there is a side nav */
  showNav: boolean
  trigger: ReactElement
  children: ReactNode
}) {
  const [theme, setTheme] = useState<ThemePreference>(themePreference)
  const choose = (next: ThemePreference) => {
    applyTheme(next)
    setTheme(next)
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={trigger}>{children}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Theme</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={theme} onValueChange={(v) => choose(v as ThemePreference)}>
            <DropdownMenuRadioItem value="light">Light</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="dark">Dark</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
        {showNav && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Side nav</DropdownMenuLabel>
              <DropdownMenuRadioGroup value={nav} onValueChange={(v) => onNav(v as NavMode)}>
                <DropdownMenuRadioItem value="hover">Show on hover</DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="expanded">Always expanded</DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="collapsed">Collapsed</DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

export function AppShell({
  route,
  agentName,
  agentDid,
  deployment,
  root,
  ceiling,
  online,
  mailboxCount,
  holds,
  syncHealth,
  error,
  onDismissError,
  onReconnect,
  children,
}: {
  route: Route
  agentName: string | null
  agentDid: string | null
  deployment: DeploymentView | null
  root?: string | null
  ceiling?: string | null
  online: boolean
  mailboxCount: number
  holds?: Set<string>
  syncHealth?: SyncHealthView | null
  /** a shell error, shown as a banner over the canvas until dismissed */
  error?: string | null
  onDismissError?: () => void
  onReconnect?: () => Promise<void>
  children: ReactNode
}) {
  const [nav, setNav] = useState<NavMode>(navPreference)
  const [menuOpen, setMenuOpen] = useState(false)
  const wide = useMediaQuery('(min-width: 768px)')
  const onNav = (mode: NavMode) => {
    saveNavPreference(mode)
    setNav(mode)
  }
  /* settings live at the foot of the nav: an icon on the rail, a row in the panel */
  const railSettings = (
    <SettingsMenu
      nav={nav}
      onNav={onNav}
      showNav={wide}
      trigger={
        <button
          type="button"
          aria-label="Settings"
          className="grid size-8 place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground"
        />
      }
    >
      <SlidersHorizontal className="size-4" />
    </SettingsMenu>
  )
  const panelSettings = (
    <SettingsMenu
      nav={nav}
      onNav={onNav}
      showNav={wide}
      trigger={
        <button
          type="button"
          className="mx-3 flex h-8 w-[calc(100%-1.5rem)] items-center gap-2 rounded-lg border border-transparent pr-2 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        />
      }
    >
      <span className="grid size-[30px] shrink-0 place-items-center">
        <SlidersHorizontal className="size-4" />
      </span>
      <span className="min-w-0 flex-1 truncate">Settings</span>
    </SettingsMenu>
  )
  return (
    <div
      className="viewport-frame grid grid-rows-[auto_1fr] bg-background text-foreground"
      data-testid="app-shell"
    >
      <header className="flex h-12 items-center gap-4 px-4">
        {/* below md the rail is gone; the menu opens the same panel as a sheet */}
        <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
          <SheetTrigger
            render={
              <Button variant="ghost" size="icon-sm" className="md:hidden" aria-label="Menu" />
            }
          >
            <Menu />
          </SheetTrigger>
          <SheetContent
            side="left"
            className="flex w-72 flex-col gap-2 border-border/60 bg-raised pt-4 pb-2"
            onClickCapture={(e) => {
              /* a link in the panel navigates; the sheet goes with it */
              if ((e.target as HTMLElement).closest('a[href]')) setMenuOpen(false)
            }}
          >
            <SheetTitle className="sr-only">Navigation</SheetTitle>
            <NavPanel
              route={route}
              agentName={agentName}
              agentDid={agentDid}
              deployment={deployment}
              online={online}
              mailboxCount={mailboxCount}
              holds={holds}
              settings={panelSettings}
            />
          </SheetContent>
        </Sheet>
        <a
          href={href({ name: 'agents' })}
          aria-label="Agents"
          className="grid size-7 place-items-center rounded-md bg-ink text-background"
        >
          <Mark className="h-3" />
        </a>
        <Breadcrumb>
          <BreadcrumbList>
            <BreadcrumbItem>
              <BreadcrumbLink href={href({ name: 'agents' })}>Agents</BreadcrumbLink>
            </BreadcrumbItem>
            <BreadcrumbSeparator />
            <BreadcrumbItem>
              <BreadcrumbPage>{agentName ?? '…'}</BreadcrumbPage>
            </BreadcrumbItem>
          </BreadcrumbList>
        </Breadcrumb>
        <span className="ml-auto" />
        <SyncHealth syncHealth={syncHealth} />
      </header>
      <div
        className={cn(
          'grid min-h-0',
          'grid-cols-[1fr]',
          nav === 'expanded' ? 'md:grid-cols-[16.5rem_1fr]' : 'md:grid-cols-[3.5rem_1fr]',
        )}
      >
        <div className="hidden min-h-0 md:block">
          <RailFlyout
            route={route}
            agentName={agentName}
            agentDid={agentDid}
            deployment={deployment}
            online={online}
            mailboxCount={mailboxCount}
            holds={holds}
            mode={nav}
            settings={panelSettings}
          >
            <nav className="flex h-full flex-col items-center gap-2 pt-4">
              {/* the agent: its avatar opens the agent's configuration; a card on hover */}
              {(() => {
                const link = (
                  <a
                    href={
                      agentDid
                        ? href({ name: 'agent', agentDid, section: 'agent' })
                        : href({ name: 'agents' })
                    }
                    aria-label={`${agentName ?? 'Agent'} configuration`}
                    aria-current={route.name === 'agent' ? 'page' : undefined}
                    className={cn(
                      'relative mb-2 block size-7 rounded-full ring-1 ring-border ring-offset-2 ring-offset-background transition-shadow hover:ring-muted-foreground',
                      route.name === 'agent' && 'ring-2 ring-ink',
                    )}
                  >
                    <AgentAvatar name={agentName ?? 'Agent'} className="size-7" />
                    <span
                      className={cn(
                        'absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-background',
                        online ? 'bg-brand' : 'bg-border',
                      )}
                      aria-hidden="true"
                    />
                  </a>
                )
                return deployment ? (
                  <AgentHoverCard deployment={deployment} root={root} ceiling={ceiling}>
                    {link}
                  </AgentHoverCard>
                ) : (
                  link
                )
              })()}
              <div className="mb-1 h-px w-5 bg-border" />
              <RailItem
                label="New session"
                to={{ name: 'session', sessionId: null }}
                active={route.name === 'session' && route.sessionId === null}
              >
                <Plus className="size-4" />
              </RailItem>
              <RailItem
                label="Mailbox"
                to={{ name: 'mailbox' }}
                active={route.name === 'mailbox'}
                count={mailboxCount}
              >
                <Inbox className="size-4" />
              </RailItem>
              <RailItem
                label="Sessions"
                to={{ name: 'sessions' }}
                active={
                  route.name === 'sessions' ||
                  (route.name === 'session' && route.sessionId !== null)
                }
              >
                <ScrollText className="size-4" />
              </RailItem>
              <div className="mt-auto pb-2">{railSettings}</div>
            </nav>
          </RailFlyout>
        </div>
        <main className="relative min-h-0 overflow-hidden">
          {error && (
            <div
              role="alert"
              className="absolute inset-x-6 top-3 z-30 flex items-center gap-3 rounded-2xl border border-destructive/30 bg-raised px-4 py-2.5 shadow-md"
            >
              <CircleAlert className="size-4 shrink-0 text-destructive" />
              <p className="min-w-0 flex-1 truncate text-sm">{error}</p>
              <Button size="sm" variant="outline" onClick={() => void onReconnect?.()}>
                Reconnect
              </Button>
              <Button size="icon-xs" variant="quiet" aria-label="Dismiss" onClick={onDismissError}>
                <X />
              </Button>
            </div>
          )}
          {children}
        </main>
      </div>
    </div>
  )
}
