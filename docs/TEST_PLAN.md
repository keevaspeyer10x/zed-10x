# Zed 10x Installed Agent and Session Test Plan

Status: executed; 5/5 rows passed on the exact installed candidate; merged-revision reinstall and canary remain<br>
Generated: 2026-09-01T10:38:48.377Z<br>
North star: Keeva can choose any visible External Agent with an unambiguous host/profile label, start it in the current project, switch among agents without hidden ownership, understand genuine provider unavailability, and recover a persistent Intrepid session after transport loss without losing work.

## Scope

This plan covers the **External Agents** section of the installed Zed 10x picker. It does not claim that native Zed Agent, Terminal, or Add More Agents are external-agent variants.

## Surface × journey closure

| Installed surface | Visible variants | Host inventory | New session and project outcome | Switch and return | Cleanup | Persistent recovery |
|---|---:|---|---|---|---|---|
| Mac local | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | N/A |
| Intrepid persistent | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | Installed boundary |
| Intrepid ordinary | 6 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | N/A |

Every applicable variant × journey cell is covered. The plan contains 32 direct cells and 64 mechanically equivalent cells whose shared product path has a current deterministic regression; no provider-health result is inferred from a sibling profile.

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
1. Select the direct representative for every installed surface through the current installed picker
2. Create a new session and reach a project-bound ready or precise external-readiness outcome
3. Bind sibling variants only when their route-specific inputs are byte-identical and the shared mechanism has a current deterministic regression
4. Retain the complete predecessor route matrix as historical support rather than relabelling it as current execution

Pass criteria:
- three current surface representatives pass
- all 22 variants are direct or mechanically equivalent
- zero product failures
- no substitution

### ZED-ACP-CLEANUP-003: Closing and failure leave route-appropriate process and journal state

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-TERMINATION-CLEANUP<br>
**Variants:** 22 direct variants<br>
**Expected:** No leaked ordinary ownership and no accidental destruction of persistent recovery state.

Steps:
1. Close current direct representative sessions on every installed surface
2. Census ordinary owned process groups
3. Verify persistent attachments detached and lane services remain healthy
4. Bind sibling variants only through byte-identical route inputs and the current shared teardown regressions

Pass criteria:
- ordinary cleanup green
- persistent detach green
- journal and service health green

### ZED-ACP-RECOVERY-004: A killed persistent lane recovers the original session through the installed Retry action

**Surfaces:** SURFACE-ACP-INTREPID-PERSISTENT<br>
**Journey:** JOURNEY-PERSISTENT-SESSION-RECOVERY<br>
**Variants:** 8 direct variants<br>
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

### ZED-ACP-SWITCH-005: Switch-and-return releases replaceable drafts and isolates retained sessions

**Surfaces:** SURFACE-ACP-MAC-LOCAL, SURFACE-ACP-INTREPID-PERSISTENT, SURFACE-ACP-INTREPID-ORDINARY<br>
**Journey:** JOURNEY-AGENT-SWITCH-AND-RETURN<br>
**Variants:** 22 direct variants<br>
**Expected:** Every installed host surface supports switch-and-return across empty and retained states without a ghost draft, stale connection, or session-ownership collision.

Steps:
1. Exercise both a ready empty draft and a non-empty retained thread on a direct representative for each installed host surface
2. Switch to a representative from another independently selectable agent class
3. Switch back to the first representative without restarting Zed
4. Verify ready composer state, retained history usability, no hidden ownership error, and route-appropriate cleanup
5. Bind sibling variants by the shared replacement path after each was independently exercised by ZED-ACP-SESSION-002

Pass criteria:
- Mac Codex -> Cursor -> Codex succeeds
- Intrepid persistent Codex -> ordinary Cursor -> persistent Codex succeeds
- old empty draft entity is dropped deterministically
- retained non-empty single-owner threads use distinct connections
- mechanical equivalence is explicit for sibling variants

## Execution order

1. Run deterministic inventory and Retry regressions.
2. Capture exact installed Mac and Intrepid runtime identities.
3. Exercise installed picker inventory and every visible variant.
4. Exercise cleanup semantics.
5. Exercise representative switch-and-return journeys and bind sibling variants by the mechanically verified shared replacement path.
6. Restart one real persistent lane while attached and recover the original session through Retry.
7. Bind content-free evidence to the exact tested revision and validate the lifecycle artifacts.
