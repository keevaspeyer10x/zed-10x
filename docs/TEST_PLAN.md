# Zed 10x Installed Agent and Session Test Plan

Status: execution plan finalized for exact-candidate installed validation<br>
Generated: 2026-08-31T19:28:50.299Z<br>
North star: Keeva can choose any visible External Agent with an unambiguous host/profile label, start it in the current project, understand genuine provider unavailability, and recover a persistent Intrepid session after transport loss without losing work.

## Scope

This plan covers the **External Agents** section of the installed Zed 10x picker. It does not claim that native Zed Agent, Terminal, or Add More Agents are external-agent variants.

## Surface × journey closure

| Installed surface | Visible variants | Host inventory | New session and project outcome | Cleanup | Persistent recovery |
|---|---:|---|---|---|---|
| Mac local | 8 | Installed boundary | Installed boundary | Installed boundary | N/A |
| Intrepid persistent | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary |
| Intrepid ordinary | 6 | Installed boundary | Installed boundary | Installed boundary | N/A |

All 22 visible variants receive direct coverage for inventory, new-session outcome, and cleanup. Persistent recovery is directly exercised with `Codex (Intrepid, primary)` and mechanically covers the other seven persistent variants because they share the same `zed-acp-session-attach` transport, Retry state machine, `session/load` path, and durable-journal contract; their route-specific launch inputs remain directly covered by the complete inventory/session/cleanup matrix. The resulting plan contains 67 direct and 7 mechanically equivalent variant × journey cells.

## Tier 0 rows

### ZED-ACP-INVENTORY-001: Installed picker is complete, host-scoped, consistently labelled, and duplicate-free

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-HOST-SCOPED-INVENTORY<br>
**Variants:** 22 direct variants<br>
**Expected:** All 22 host-valid External Agent variants are visible exactly once on the correct project host.

Steps:
1. Open a Mac-local project and inspect External Agents
2. Open an Intrepid project and inspect External Agents
3. Compare every visible entry to the checked inventory and host-label grammar

Pass criteria:
- exact label sets match
- no duplicates
- no off-host entries

### ZED-ACP-SESSION-002: Every visible External Agent starts the named route and reaches a project-bound outcome

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-NEW-SESSION-PROJECT-OUTCOME<br>
**Variants:** 22 direct variants<br>
**Expected:** Every variant has a product-green terminal outcome and healthy routes remain usable after a failed provider.

Steps:
1. Select every visible External Agent once through the installed picker
2. Create a new session and request the exact project sentinel
3. Record a project-bound pass or precise external-readiness outcome
4. After an unavailable route, start a healthy route without restarting Zed

Pass criteria:
- 22/22 variants attempted
- zero product failures
- no substitution
- healthy-after-failure succeeds

### ZED-ACP-CLEANUP-003: Closing and failure leave route-appropriate process and journal state

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-TERMINATION-CLEANUP<br>
**Variants:** 22 direct variants<br>
**Expected:** No leaked ordinary ownership and no accidental destruction of persistent recovery state.

Steps:
1. Close, cancel, and fail representative sessions after the complete route matrix
2. Census ordinary owned process groups
3. Verify persistent attachments detached and lane services remain healthy

Pass criteria:
- ordinary cleanup green
- persistent detach green
- journal and service health green

### ZED-ACP-RECOVERY-004: A killed persistent lane recovers the original session through the installed Retry action

**Surfaces:** SURFACE-ACP-INTREPID-PERSISTENT<br>
**Journey:** JOURNEY-PERSISTENT-SESSION-RECOVERY<br>
**Variants:** 1 direct variant plus 7 mechanically equivalent persistent variants<br>
**Expected:** The screenshot failure becomes a finite recoverable interruption rather than a lost or blank session.

Steps:
1. Attach to a completed persistent Intrepid session
2. Restart its exact user service while Zed remains attached
3. Observe a recoverable failure and invoke Retry
4. Verify the same session and project, then continue once

Pass criteria:
- fresh transport
- same session ID
- same history
- same project
- continuation passes

## Execution order

1. Run deterministic inventory and Retry regressions.
2. Capture exact installed Mac and Intrepid runtime identities.
3. Exercise installed picker inventory and every visible variant.
4. Exercise cleanup semantics.
5. Restart one real persistent lane while attached and recover the original session through Retry.
6. Bind content-free evidence to the exact tested revision and validate the lifecycle artifacts.
