# Zed 10x dogfooding canary telemetry

This collector is the first, deliberately small observability slice for the
Zed 10x reliability campaign. It records content-free lifecycle and resource
events locally so normal Zed can act as the control cohort and Zed 10x as the
canary cohort. Telemetry must never be on the critical path for editor launch,
ACP, SSH, or reconnection.

Tracking issue: <https://github.com/keevaspeyer10x/zed-10x/issues/15>

## Architecture altitude

- Instance: isolated hang samples and anecdotal ACP disconnect reports.
- Class: reliability claims without a shared, privacy-safe event contract or
  comparable control/canary denominators.
- Generator: no bounded correlation-aware lifecycle stream that promotes
  repeated failures into durable incidents.
- Minimal-correct altitude: an allow-listed local event envelope, W3C Trace
  Context identifiers, an OpenTelemetry-compatible attribute vocabulary,
  bounded retention, and an explicit comparison/incident contract.

Internal prior art checked: Zed's `telemetry` and `telemetry_events` crates,
the ACP connection `telemetry_id`, existing Agent Thread/Turn telemetry, the
secure remote environment transport, and the campaign's existing hang and
remote-bootstrap samples. External prior art checked: W3C Trace Context and
the OpenTelemetry Logs Data Model and system semantic conventions. This slice
adopts their interface concepts without adding an OTLP SDK or network exporter.

## Data contract

Each JSONL record contains only:

- timestamp and observed timestamp;
- allow-listed event name and severity;
- 32-hex trace ID and 16-hex span ID;
- app cohort (`zed` or `zed10x`), lane (`local`, `intrepid`, or `unknown`),
  app/build version, and optional source commit;
- numeric durations and resource samples;
- bounded classification tokens such as provider alias, failure class,
  continuity trigger/result, and process state;
- SHA-256 project or session identifiers when correlation needs them.

The collector rejects unknown fields. It does not read or store prompts,
responses, tool payloads, file contents, raw log lines, process arguments,
environment dumps, access tokens, provider credentials, repository names, or
raw project/session paths. The normal-Zed observer reads `ps` using only PID
and executable path, compares the executable to one exact allow-listed path,
and never persists the process table.

## Events

The allow-list covers:

- app launch, exit, restart, hang, crash, and forced quit;
- project-open request and completion;
- remote bootstrap start, completion, and failure;
- ACP start, completion, disconnect, and reconnect;
- continuity check, success, and failure across restart, SSH loss, or laptop
  closure;
- resource samples and pressure classification.

The launcher emits app and resource events immediately. The `record` command
is the stable hook for the local ACP smoke harness, remote bootstrap, Apex, and
the persistent controller until those components emit the same envelope
directly. `TRACEPARENT`, `ZED_10X_TRACEPARENT`, and
`ZED_10X_CORRELATION_ID` are exported by the Zed 10x launcher so child
processes can carry the correlation boundary where their contracts permit it.

## Storage, retention, and redaction

Default store:

```text
~/Library/Application Support/Zed 10x/dogfood-canary/
```

The directory is mode `0700`; event and incident files are mode `0600`.
Daily event files are retained for 14 days and bounded to 8 MiB each. Pruning,
incident evaluation, and comparison generation are best-effort. Any failure
returns without affecting Zed.

Repeated ACP disconnects, app instability, remote-bootstrap failures,
continuity failures, and sustained resource pressure create deduplicated
`incidents.jsonl` records linked to issue #15. This is the durable local
incident queue; later campaign slices may promote those records through Apex
to a dedicated GitHub incident while preserving the same trace ID and failure
class.

## Disable and rollback

Either control disables collection immediately:

```sh
export ZED_10X_TELEMETRY_DISABLED=1
node script/zed-10x-canary.mjs disable
```

The second command creates a `DISABLED` sentinel in the store. Re-enable with:

```sh
node script/zed-10x-canary.mjs enable
```

Removing the canary launcher and collector from the Zed 10x bundle rolls back
instrumentation. Normal Zed's bundle, profile, database, and update channel are
never modified. The control observer is a separate user LaunchAgent and can be
unloaded independently.

## Operator evidence

Generate the comparison receipt with:

```sh
node script/zed-10x-canary.mjs compare --output comparison.json
```

The receipt separates cohort, lane, and build and reports session count,
observed minutes, ACP completions/disconnects/reconnects, continuity outcomes,
hang/crash/forced-quit counts, peak RSS, and failure events. It reports
`insufficient_evidence` with low confidence until both cohorts have at least
five launches and 30 observed minutes. Only then can it state whether Zed 10x
is materially more reliable, materially less reliable, or not materially
different for the measured lane/build.

## Current boundary

This first slice intentionally does not send telemetry over the network or
inspect arbitrary Zed logs. Automatic ACP/remote/continuity hooks in Apex,
remote bootstrap, and the persistent Intrepid controller remain part of the
same issue and campaign. The external `record` hook allows the first local ACP
smoke to be correlated now without holding the recovery build behind those
deeper integrations.
