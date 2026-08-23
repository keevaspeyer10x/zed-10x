# Zed 10x External-Agent Surface/Journey Test Plan

Status: generated from fresh discovery on `0a67f0e55990e52791b28d90ccdd17cfff80a431`.

Scope: the installed Zed 10x external-agent subsystem for Mac-local projects and Mac-to-Intrepid remote projects. This is deliberately not a claim about every upstream Zed feature.

## Coverage closure

Every critical journey below applies to both supported product surfaces. A component test, a direct host-command canary, or a pass on the sibling surface does not close another cell.

| Journey | Mac-local installed app | Mac app → Intrepid remote project |
|---|---|---|
| Cold open inventory | ACP-SMOKE-001 | ACP-SMOKE-001 |
| Launch and read project bytes | ACP-JOURNEY-002 | ACP-JOURNEY-002 |
| Settings refresh | ACP-JOURNEY-003 | ACP-JOURNEY-003 |
| Reconnect | ACP-JOURNEY-004 | ACP-JOURNEY-004 |
| Failure classification | ACP-NEG-005 | ACP-NEG-005 |
| Cleanup | ACP-OPS-006 | ACP-OPS-006 |

## Section 0: critical-path smoke

### ACP-SMOKE-001 — cold-open inventory is timing-independent

Add a receiver-driven inventory snapshot over the remote protocol. Exercise both legal orderings: the server inventory exists before the client registers, and the client is ready before a later settings update. Pass only when exact names arrive without a fixed sleep or unrelated mutation.

### ACP-JOURNEY-002 — representative agent reads real project bytes

Use two complementary executable oracles. The tracked ACP canary must require a completed ACP tool call, withhold the expected sentinel digest from the prompt, bind output to the real project cwd and known sentinel, observe a terminal response, reject prompt echo and marker-only text, and grant no route-requested permissions. The installed-product UAT must separately launch that route through the real Zed client/remote-server seam; the direct route canary cannot substitute for that assembled-product journey.

### ACP-JOURNEY-004 — reconnect restores current inventory

Disconnect while an inventory request may be in flight, change settings while the transport is disconnected, and rejoin the same project. Pass only when the receiver requests and applies the current snapshot, a newer push invalidates an older response, the latest request wins, and stale names are absent.

## Section 3: integration

### ACP-JOURNEY-003 — settings refresh is idempotent

Apply add/remove/duplicate inventory updates. Assert exact convergence, no duplicate route, and one current inventory.

## Section 11: failure and edge cases

### ACP-NEG-005 — failures remain explicit

Fixture missing executable, authentication, capacity, permission requests, prompt echo, and nonterminal timeout outcomes. Ensure none can satisfy the journey oracle. A timeout must terminate the owned process group.

## Section 13: operational readiness

### ACP-OPS-006 — cleanup is observed

For normal completion and timeout, retain the launched PID identity and verify the process group is gone. Do not infer cleanup from a wrapper exit alone.

## Release decision

The subsystem is not release-ready until all 12 applicable surface × journey cells have executable assembled-product evidence. Deterministic protocol tests can establish message ordering and convergence; installed-app Mac-local and Mac→Intrepid journey evidence still requires an explicitly authorized app-control UAT action.
