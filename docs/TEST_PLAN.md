# Zed 10x Installed Agent and Session Test Plan

Status: executed; production-readiness candidate pending exact protected merge and post-merge install<br>
Generated: 2026-09-02T14:14:12.000Z<br>
North star: Keeva can choose any visible External Agent with an unambiguous host/profile label, start it in the current project, switch among agents without hidden ownership, understand genuine provider unavailability, and recover a persistent Intrepid session after transport loss without losing work.

## Scope

This plan covers the **External Agents** section of the installed Zed 10x picker. It does not claim that native Zed Agent, Terminal, or Add More Agents are external-agent variants.

## Surface × journey closure

| Installed surface | Visible variants | Host inventory | New session and project outcome | Switch and return | Cleanup | Intrepid recovery |
|---|---:|---|---|---|---|---|
| Mac local | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | N/A |
| Intrepid persistent | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | Installed boundary |
| Intrepid ordinary | 6 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | Installed boundary |

All 22 visible variants receive direct coverage for inventory, launch outcome, and cleanup. The resulting plan contains 66 direct route-specific variant × journey cells and does not infer one profile or provider from another. Stateful switch and Intrepid recovery are separate surface-level journeys exercised on the named representatives whose shared product state they target. Recovery covers both persistent journal restoration and ordinary dedicated-transport replacement.

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
1. Select every visible route through the current installed provider-launch boundary
2. For each route, create a new session and reach a project-bound ready or precise external-readiness outcome
3. Reject a missing receipt, product-origin failure, substitution, or incomplete matrix

Pass criteria:
- all 22 current variants have direct route receipts
- zero product failures
- no substitution

### ZED-ACP-CLEANUP-003: Closing and failure leave route-appropriate process and journal state

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-TERMINATION-CLEANUP<br>
**Variants:** 22 direct variants<br>
**Expected:** No leaked ordinary ownership and no accidental destruction of persistent recovery state.

Steps:
1. Close every current route launched by the complete route matrix
2. Require every route receipt to prove its owned process group is gone
3. Verify persistent attachments detached and lane services remain healthy

Pass criteria:
- ordinary cleanup green
- persistent detach green
- journal and service health green

### ZED-ACP-RECOVERY-004: Intrepid restart recovery preserves sessions and replaces dedicated transports exactly once

**Surfaces:** SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-PERSISTENT-SESSION-RECOVERY<br>
**Variants:** 2 direct variants<br>
**Expected:** Intrepid disruption is finite: persistent work reloads, and ordinary agents continue through one clean transport replacement.

Steps:
1. Attach to a completed persistent Intrepid session
2. Restart its exact user service while Zed remains attached
3. Verify Retry restores the same session and project
4. Start one ordinary dedicated Intrepid route in the installed app
5. Terminate the exact remote-server process and observe one automatic replacement
6. Verify the old dedicated transport exits, exactly one replacement remains, and app close leaves no owned agent residue

Pass criteria:
- persistent route uses a fresh transport with the same session ID, history, and project
- ordinary route has exactly one replacement transport
- superseded ordinary transport is gone
- normal app close leaves no owned agent residue

### ZED-ACP-SWITCH-005: Switch-and-return releases replaceable drafts and isolates retained sessions

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-AGENT-SWITCH-AND-RETURN<br>
**Variants:** 4 direct variants<br>
**Expected:** Every installed host surface supports switch-and-return across empty and retained states without a ghost draft, stale connection, or session-ownership collision.

Steps:
1. Exercise both a ready empty draft and a non-empty retained thread on a direct representative for each installed host surface
2. Switch to a representative from another independently selectable agent class
3. Switch back to the first representative without restarting Zed
4. Verify ready composer state, retained history usability, no hidden ownership error, and route-appropriate cleanup

Pass criteria:
- Mac Codex -> Cursor -> Codex succeeds
- Intrepid persistent Codex -> ordinary Cursor -> persistent Codex succeeds
- old empty draft entity is dropped deterministically
- retained non-empty single-owner threads use distinct connections

## Execution order

1. Run deterministic inventory and Retry regressions.
2. Capture exact installed Mac and Intrepid runtime identities.
3. Exercise installed picker inventory and every visible variant.
4. Exercise cleanup semantics.
5. Exercise the named representative switch-and-return journeys as surface-level state transitions.
6. Restart one real persistent lane and one ordinary remote editor server while attached; recover the original persistent session and prove exactly one ordinary replacement transport.
7. Bind content-free evidence to the exact tested revision and validate the lifecycle artifacts.
