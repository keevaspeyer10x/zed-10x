# Zed 10x Test Plan Analysis

The fresh lifecycle is complete and supports a production-ready candidate for Keeva's local, ad-hoc-signed Zed 10x installation. All four critical rows passed against exact installed artifacts: 4/4 rows, 9/9 requirements, three product surfaces, four user journeys, and all 22 host-valid selectable variants.

## What changed the confidence

The previous false-positive mechanism was proxy substitution: configuration and direct ACP/provider canaries were treated as proof of UI selectability, assembled launch wrappers, host-local execution, cleanup, and restart recovery. The new closure is mechanical across the real product boundary:

- the Mac picker exposed exactly eight Mac entries and no Intrepid entries;
- the Intrepid picker exposed exactly eight persistent and six ordinary entries and no Mac entries;
- every visible variant was exercised once through installed Zed, with no product-origin launch failure;
- representative final-runtime sessions became usable on both hosts;
- normal quit left zero exact Zed runtime processes on either host;
- a real restart of zed-acp-session-host@codex--primary.service produced one finite recoverable failure, one Retry restored the original session/history/project, and a post-recovery continuation completed.

The recovery evidence binds the exact remote artifact, before/after systemd invocation identities, a content-free session identity digest, project digest, and the final cleanup census. The full lifecycle validator passed with no failures or warnings.

## Defects found and closed

1. Retry reused an exited connection instead of opening a fresh transport before session/load.
2. Remote env-exec descendants could survive both transport-EOF and leader-exit orderings.
3. The local macOS bundler continued into distribution staging after moving the installed app.

Deterministic regressions now cover both cleanup orderings and the original-session Retry contract. Installed UAT covers the assembled host and UI seams that unit tests cannot establish.

## Residual boundaries

- Public Developer ID signing and notarization remain out of scope because this app is for Keeva's private use.
- Provider authentication, capacity, or account availability can still make an individual agent unavailable; those outcomes must be explicit but are not Zed product failures.
- The final exact candidate still requires the normal independent review, protected PR, required CI, merge, and merged-main install/canary gates before delivery is complete.
