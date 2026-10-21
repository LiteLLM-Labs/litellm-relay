import assert from "node:assert/strict"
import { test } from "node:test"

import { keyEvents } from "./eventKeys.ts"

const tunnel = "371ec2c1-f72d-4749-906a-64f10c69aaa0"

test("lifecycle events that share one event_id get distinct keys", () => {
  const keys = keyEvents([
    { event: "connect", event_id: tunnel, host: "api.anthropic.com" },
    { event: "connect_closed", event_id: tunnel, host: "api.anthropic.com" },
  ]).map((item) => item.key)
  assert.equal(new Set(keys).size, 2)
})

test("events without an event_id that look alike still get distinct keys", () => {
  const skipped = { event: "collector_skipped", reason: "no key" }
  const keys = keyEvents([skipped, { ...skipped }, { ...skipped }]).map((item) => item.key)
  assert.equal(new Set(keys).size, 3)
})

test("keys are stable across polls when the log only grows", () => {
  const first = [
    { event: "relay_started" },
    { event: "connect", event_id: tunnel },
    { event: "connect_closed", event_id: tunnel },
  ]
  const before = keyEvents(first).map((item) => item.key)
  const after = keyEvents([...first, { event: "connect", event_id: "later" }]).map((item) => item.key)
  assert.deepEqual(after.slice(0, 3), before)
})

test("a cleared log keys only what the daemon returns", () => {
  const keyed = keyEvents([{ event: "relay_log_cleared", listen: "127.0.0.1:4142" }])
  assert.equal(keyed.length, 1)
  assert.equal(keyed[0]?.event.event, "relay_log_cleared")
})
