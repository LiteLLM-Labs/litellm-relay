export type RelayEventLike = Record<string, unknown>

export type KeyedEvent<T extends RelayEventLike> = {
  event: T
  key: string
}

function text(value: unknown): string {
  if (value === null || value === undefined) {
    return ""
  }
  return String(value)
}

/// The identity Relay gives an event: its `event_id` when it has one, else the
/// fields that describe it. Lifecycle events of one tunnel (`connect`,
/// `connect_closed`, ...) share an `event_id` on purpose, so the event kind is
/// part of the key.
function baseKey(event: RelayEventLike): string {
  const identity = text(event.event_id) || text(event.captured_at) || text(event.timestamp)
  return [identity, text(event.event), text(event.host), text(event.path), text(event.status_code)].join("\u001f")
}

/// Gives every event in log order a key that is unique within the list and
/// stable across polls, so React rows reconcile one to one, the selected row
/// is the one that was clicked, and a cleared log leaves no stale rows behind.
export function keyEvents<T extends RelayEventLike>(events: readonly T[]): KeyedEvent<T>[] {
  const seen = new Map<string, number>()
  return events.map((event) => {
    const base = baseKey(event)
    const occurrence = seen.get(base) ?? 0
    seen.set(base, occurrence + 1)
    return { event, key: occurrence === 0 ? base : `${base}\u001f${occurrence}` }
  })
}
