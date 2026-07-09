import { useCallback, useEffect, useMemo, useState } from "react"
import type { ComponentType, ReactNode } from "react"
import {
  Activity,
  BadgeCheck,
  CircleAlert,
  Clipboard,
  Database,
  FileJson,
  RefreshCw,
  ShieldCheck,
  Trash2,
  Waypoints,
  Wifi,
} from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Separator } from "@/components/ui/separator"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

type RelayStatus = {
  listen?: string
  log_path?: string
  ai_domains?: string[]
  notion_domains?: string[]
  capture_payloads?: boolean
  mitm_ca_path?: string | null
  shadow_enabled?: boolean
  gateway_url?: string
  events_loaded?: number
  runtime?: string
}

type RelayEvent = Record<string, unknown>

const statusCards = [
  {
    key: "capture",
    label: "Payload capture",
    icon: ShieldCheck,
  },
  {
    key: "gateway",
    label: "Gateway ingest",
    icon: Database,
  },
  {
    key: "events",
    label: "Events loaded",
    icon: Activity,
  },
  {
    key: "runtime",
    label: "Runtime",
    icon: Wifi,
  },
] as const

const eventColumns = ["event", "app", "host", "path", "status", "ingest"]

function App() {
  const [status, setStatus] = useState<RelayStatus | null>(null)
  const [events, setEvents] = useState<RelayEvent[]>([])
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [copyState, setCopyState] = useState<string | null>(null)

  const refresh = useCallback(async (signal?: AbortSignal) => {
    setError(null)
    const [statusResponse, eventsResponse] = await Promise.all([
      fetch("/api/status", { signal }),
      fetch("/api/events?limit=200", { signal }),
    ])

    if (!statusResponse.ok || !eventsResponse.ok) {
      throw new Error("Relay API returned an error")
    }

    const statusPayload = (await statusResponse.json()) as RelayStatus
    const eventsPayload = (await eventsResponse.json()) as {
      events?: RelayEvent[]
    }

    setStatus(statusPayload)
    setEvents(eventsPayload.events ?? [])
    setSelectedEventId((current) => {
      if (current && eventsPayload.events?.some((event) => getEventId(event) === current)) {
        return current
      }
      return getEventId(eventsPayload.events?.at(-1))
    })
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    refresh(controller.signal)
      .catch((refreshError: unknown) => {
        if (!controller.signal.aborted) {
          setError(refreshError instanceof Error ? refreshError.message : "Unable to load Relay state")
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false)
        }
      })

    const interval = window.setInterval(() => {
      refresh().catch((refreshError: unknown) => {
        setError(refreshError instanceof Error ? refreshError.message : "Unable to refresh Relay state")
      })
    }, 5000)

    return () => {
      controller.abort()
      window.clearInterval(interval)
    }
  }, [refresh])

  const selectedEvent = useMemo(
    () => events.find((event) => getEventId(event) === selectedEventId) ?? events.at(-1) ?? null,
    [events, selectedEventId]
  )

  const stats = useMemo(() => {
    const requests = events.filter((event) => asText(event.event) === "http_request").length
    const responses = events.filter((event) => asText(event.event) === "http_response").length
    const ingestAttempts = events.filter((event) => asText(event.event) === "gateway_ingest")
    const ingestOk = ingestAttempts.filter((event) => event.ok === true).length
    const errors = events.filter((event) => asText(event.event).includes("failed") || event.ok === false).length

    return {
      requests,
      responses,
      ingestAttempts: ingestAttempts.length,
      ingestOk,
      errors,
    }
  }, [events])

  const clearEvents = async () => {
    await fetch("/api/events/clear", { method: "POST" })
    await refresh()
  }

  const copyText = async (label: string, value?: string | null) => {
    if (!value) {
      return
    }
    await navigator.clipboard.writeText(value)
    setCopyState(label)
    window.setTimeout(() => setCopyState(null), 1500)
  }

  return (
    <main className="min-h-screen bg-background text-foreground">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4 px-4 py-4 sm:px-6 lg:px-8">
        <header className="flex flex-col gap-4 rounded-xl border bg-card px-4 py-4 shadow-sm sm:flex-row sm:items-center sm:justify-between">
          <div className="flex min-w-0 items-center gap-3">
            <div className="flex size-10 shrink-0 items-center justify-center rounded-lg border bg-muted font-mono text-sm font-semibold">
              LR
            </div>
            <div className="min-w-0">
              <h1 className="truncate text-lg font-semibold leading-tight">LiteLLM Relay</h1>
              <p className="mt-1 text-sm text-muted-foreground">
                Local traffic capture, redaction, and Gateway log ingestion.
              </p>
            </div>
          </div>
          <div className="flex flex-wrap gap-2">
            <Badge variant={status?.capture_payloads ? "default" : "secondary"}>
              {status?.capture_payloads ? "Capture on" : "Capture off"}
            </Badge>
            <Badge variant={status?.shadow_enabled ? "default" : "outline"}>
              {status?.shadow_enabled ? "Shadow on" : "Shadow off"}
            </Badge>
            <Badge variant="outline">{status?.listen ?? "127.0.0.1:4142"}</Badge>
          </div>
        </header>

        {error ? (
          <div className="flex items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            <CircleAlert className="size-4" />
            <span>{error}</span>
          </div>
        ) : null}

        <section className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
          {statusCards.map((card) => (
            <StatusCard key={card.key} cardKey={card.key} label={card.label} status={status} stats={stats} icon={card.icon} />
          ))}
        </section>

        <section className="grid gap-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(360px,0.75fr)]">
          <Card className="min-w-0">
            <CardHeader>
              <CardTitle>Traffic events</CardTitle>
              <CardDescription>
                CONNECT, request, response, and Gateway ingest activity from the local relay.
              </CardDescription>
              <CardAction className="flex gap-2">
                <Button variant="outline" size="sm" onClick={() => refresh()} disabled={loading}>
                  <RefreshCw className="size-4" />
                  Refresh
                </Button>
                <Button variant="destructive" size="sm" onClick={clearEvents}>
                  <Trash2 className="size-4" />
                  Clear
                </Button>
              </CardAction>
            </CardHeader>
            <CardContent>
              <div className="overflow-hidden rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      {eventColumns.map((column) => (
                        <TableHead key={column}>{column}</TableHead>
                      ))}
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {events.length === 0 ? (
                      <TableRow>
                        <TableCell colSpan={eventColumns.length} className="h-28 text-center text-muted-foreground">
                          {loading ? "Loading relay events..." : "No relay events yet."}
                        </TableCell>
                      </TableRow>
                    ) : (
                      events
                        .slice()
                        .reverse()
                        .map((event) => (
                          <TableRow
                            key={getEventId(event)}
                            data-state={getEventId(event) === getEventId(selectedEvent ?? {}) ? "selected" : undefined}
                            className="cursor-pointer"
                            onClick={() => setSelectedEventId(getEventId(event))}
                          >
                            <TableCell className="font-mono text-xs">{asText(event.event)}</TableCell>
                            <TableCell>{asText(event.app) || "unknown"}</TableCell>
                            <TableCell className="max-w-48 truncate font-mono text-xs">{asText(event.host) || "-"}</TableCell>
                            <TableCell className="max-w-56 truncate font-mono text-xs">{asText(event.path) || "-"}</TableCell>
                            <TableCell>
                              <EventStatus event={event} />
                            </TableCell>
                            <TableCell>
                              <IngestBadge event={event} />
                            </TableCell>
                          </TableRow>
                        ))
                    )}
                  </TableBody>
                </Table>
              </div>
            </CardContent>
          </Card>

          <div className="flex min-w-0 flex-col gap-4">
            <Card>
              <CardHeader>
                <CardTitle>Relay status</CardTitle>
                <CardDescription>{status?.gateway_url ?? "Gateway URL unavailable"}</CardDescription>
              </CardHeader>
              <CardContent className="grid gap-3">
                <StatusLine label="Log path" value={status?.log_path} onCopy={() => copyText("log path", status?.log_path)} />
                <StatusLine label="CA path" value={status?.mitm_ca_path ?? "payload capture disabled"} onCopy={() => copyText("CA path", status?.mitm_ca_path)} />
                <StatusLine label="PAC URL" value={`${window.location.origin}/proxy.pac`} onCopy={() => copyText("PAC URL", `${window.location.origin}/proxy.pac`)} />
                {copyState ? <p className="text-xs text-muted-foreground">Copied {copyState}</p> : null}
              </CardContent>
            </Card>

            <Card className="min-w-0">
              <CardHeader>
                <CardTitle>Event inspector</CardTitle>
                <CardDescription>
                  {selectedEvent ? `${asText(selectedEvent.event)} ${asText(selectedEvent.host)}` : "Select a row to inspect payload previews."}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <Tabs defaultValue="summary">
                  <TabsList>
                    <TabsTrigger value="summary">Summary</TabsTrigger>
                    <TabsTrigger value="json">
                      <FileJson className="size-4" />
                      JSON
                    </TabsTrigger>
                  </TabsList>
                  <TabsContent value="summary" className="mt-3">
                    <EventSummary event={selectedEvent} />
                  </TabsContent>
                  <TabsContent value="json" className="mt-3">
                    <ScrollArea className="h-[420px] rounded-lg border bg-muted/40">
                      <pre className="whitespace-pre-wrap break-words p-3 font-mono text-xs text-muted-foreground">
                        {selectedEvent ? JSON.stringify(selectedEvent, null, 2) : "{}"}
                      </pre>
                    </ScrollArea>
                  </TabsContent>
                </Tabs>
              </CardContent>
            </Card>
          </div>
        </section>

        <section className="grid gap-4 lg:grid-cols-3">
          <DomainCard title="AI domains" domains={status?.ai_domains ?? []} icon={<Waypoints className="size-4" />} />
          <DomainCard title="Notion domains" domains={status?.notion_domains ?? []} icon={<BadgeCheck className="size-4" />} />
          <Card>
            <CardHeader>
              <CardTitle>Capture totals</CardTitle>
              <CardDescription>Current local log window.</CardDescription>
            </CardHeader>
            <CardContent className="grid grid-cols-2 gap-3">
              <Metric label="Requests" value={stats.requests} />
              <Metric label="Responses" value={stats.responses} />
              <Metric label="Ingest attempts" value={stats.ingestAttempts} />
              <Metric label="Errors" value={stats.errors} />
            </CardContent>
          </Card>
        </section>
      </div>
    </main>
  )
}

type StatusCardProps = {
  cardKey: (typeof statusCards)[number]["key"]
  label: string
  status: RelayStatus | null
  stats: {
    requests: number
    responses: number
    ingestAttempts: number
    ingestOk: number
    errors: number
  }
  icon: ComponentType<{ className?: string }>
}

function StatusCard({ cardKey, label, status, stats, icon: Icon }: StatusCardProps) {
  const value =
    cardKey === "capture"
      ? status?.capture_payloads
        ? "Enabled"
        : "Disabled"
      : cardKey === "gateway"
        ? `${stats.ingestOk}/${stats.ingestAttempts || 0} ok`
        : cardKey === "events"
          ? String(status?.events_loaded ?? 0)
          : status?.runtime ?? "rust"

  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm">
          <Icon className="size-4 text-muted-foreground" />
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="font-mono text-2xl font-semibold">{value}</div>
      </CardContent>
    </Card>
  )
}

function EventStatus({ event }: { event: RelayEvent }) {
  const failed = asText(event.event).includes("failed") || event.ok === false
  const status = event.status_code ?? event.status ?? event.error
  if (failed) {
    return <Badge variant="destructive">{asText(status) || "failed"}</Badge>
  }
  if (status) {
    return <Badge variant="outline">{asText(status)}</Badge>
  }
  return <Badge variant="secondary">ok</Badge>
}

function IngestBadge({ event }: { event: RelayEvent }) {
  if (asText(event.event) !== "gateway_ingest") {
    return <span className="text-muted-foreground">-</span>
  }
  if (event.ok === true) {
    return <Badge>sent</Badge>
  }
  if (event.attempted === false) {
    return <Badge variant="secondary">skipped</Badge>
  }
  return <Badge variant="destructive">failed</Badge>
}

function StatusLine({
  label,
  value,
  onCopy,
}: {
  label: string
  value?: string | null
  onCopy: () => void
}) {
  return (
    <div className="grid gap-1">
      <div className="text-xs uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="flex min-w-0 items-center gap-2">
        <code className="min-w-0 flex-1 truncate rounded-md bg-muted px-2 py-1 text-xs">{value ?? "-"}</code>
        <Button variant="ghost" size="icon-sm" onClick={onCopy} disabled={!value}>
          <Clipboard className="size-4" />
        </Button>
      </div>
    </div>
  )
}

function EventSummary({ event }: { event: RelayEvent | null }) {
  if (!event) {
    return <div className="rounded-lg border bg-muted/40 p-4 text-sm text-muted-foreground">No event selected.</div>
  }

  const rows = [
    ["Event", asText(event.event)],
    ["App", asText(event.app) || "unknown"],
    ["Host", asText(event.host) || "-"],
    ["Path", asText(event.path) || "-"],
    ["Method", asText(event.method) || "-"],
    ["Request bytes", asText(event.request_bytes) || "-"],
    ["Response bytes", asText(event.response_bytes) || "-"],
    ["Duration", event.duration_ms ? `${asText(event.duration_ms)} ms` : "-"],
  ]

  return (
    <div className="grid gap-3">
      <div className="rounded-lg border">
        <Table>
          <TableBody>
            {rows.map(([label, value]) => (
              <TableRow key={label}>
                <TableCell className="w-36 text-muted-foreground">{label}</TableCell>
                <TableCell className="whitespace-normal break-words font-mono text-xs">{value}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
      <PreviewBlock label="Request preview" value={event.request_preview} truncated={event.request_truncated === true} />
      <PreviewBlock label="Response preview" value={event.response_preview} truncated={event.response_truncated === true} />
    </div>
  )
}

function PreviewBlock({
  label,
  value,
  truncated,
}: {
  label: string
  value: unknown
  truncated: boolean
}) {
  const text = asText(value)
  return (
    <div className="rounded-lg border bg-muted/30">
      <div className="flex items-center justify-between px-3 py-2 text-xs text-muted-foreground">
        <span>{label}</span>
        {truncated ? <Badge variant="outline">truncated</Badge> : null}
      </div>
      <Separator />
      <pre className="max-h-36 overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-xs">
        {text || "No preview captured for this event."}
      </pre>
    </div>
  )
}

function DomainCard({ title, domains, icon }: { title: string; domains: string[]; icon: ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {icon}
          {title}
        </CardTitle>
        <CardDescription>{domains.length} configured domains</CardDescription>
      </CardHeader>
      <CardContent>
        <div className="flex flex-wrap gap-2">
          {domains.length ? domains.map((domain) => <Badge key={domain} variant="outline">{domain}</Badge>) : <span className="text-sm text-muted-foreground">No domains configured.</span>}
        </div>
      </CardContent>
    </Card>
  )
}

function Metric({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-lg border bg-muted/30 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 font-mono text-xl font-semibold">{value}</div>
    </div>
  )
}

function getEventId(event?: RelayEvent | null) {
  if (!event) {
    return ""
  }
  return asText(event.event_id) || `${asText(event.event)}-${asText(event.host)}-${asText(event.path)}-${asText(event.status_code)}`
}

function asText(value: unknown) {
  if (value === null || value === undefined) {
    return ""
  }
  if (typeof value === "string") {
    return value
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value)
  }
  return JSON.stringify(value)
}

export default App
