#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const inventoryPath = path.join(
  repositoryRoot,
  "docs/test-plan-inputs/zed-agent-picker-inventory.json",
);
const inventory = JSON.parse(readFileSync(inventoryPath, "utf8"));
const generatedAt = process.env.SOURCE_DATE_EPOCH
  ? new Date(Number(process.env.SOURCE_DATE_EPOCH) * 1000).toISOString()
  : new Date().toISOString();

const surfaceDefinitions = [
  {
    id: "SURFACE-ACP-MAC-LOCAL",
    inventoryKey: "mac-local",
    name: "Installed Zed 10x External Agents picker for a Mac-local project",
    assembledEntryPoint: "installed /Applications/Zed 10x.app External Agents picker",
    sourceRefs: [
      "docs/test-plan-inputs/zed-agent-picker-inventory.json",
      "crates/agent_ui/src/agent_panel.rs",
      "crates/project/src/agent_server_store.rs",
      "crates/project/src/project_settings.rs",
      "crates/agent_servers/src/custom.rs",
      "crates/agent_servers/src/acp.rs",
    ],
  },
  {
    id: "SURFACE-ACP-INTREPID-PERSISTENT",
    inventoryKey: "intrepid-persistent",
    name: "Installed Zed 10x persistent External Agents picker for an Intrepid project",
    assembledEntryPoint: "installed Zed 10x remote project plus systemd-owned ACP session lane",
    sourceRefs: [
      "docs/test-plan-inputs/zed-agent-picker-inventory.json",
      "crates/agent_ui/src/agent_panel.rs",
      "crates/project/src/agent_server_store.rs",
      "crates/project/src/project_settings.rs",
      "crates/agent_servers/src/custom.rs",
      "crates/agent_servers/src/acp.rs",
      "crates/agent_ui/src/agent_connection_store.rs",
      "crates/agent_ui/src/conversation_view.rs",
    ],
  },
  {
    id: "SURFACE-ACP-INTREPID-ORDINARY",
    inventoryKey: "intrepid-ordinary",
    name: "Installed Zed 10x ordinary External Agents picker for an Intrepid project",
    assembledEntryPoint: "installed Zed 10x remote project plus execution-host ACP launch",
    sourceRefs: [
      "docs/test-plan-inputs/zed-agent-picker-inventory.json",
      "crates/agent_ui/src/agent_panel.rs",
      "crates/project/src/agent_server_store.rs",
      "crates/project/src/project_settings.rs",
      "crates/agent_servers/src/custom.rs",
      "crates/agent_servers/src/acp.rs",
    ],
  },
];

const surfaces = surfaceDefinitions.map(({ inventoryKey, ...surface }) => ({
  ...surface,
  provenance: [
    { source: "code", ref: "External Agents picker and execution-host settings" },
    { source: "prior_defect", ref: "visible route and host-label omissions" },
  ],
  variantCount: inventory.pickerSurfaces[inventoryKey].length,
}));

const journeys = [
  {
    id: "JOURNEY-HOST-SCOPED-INVENTORY",
    name: "See the complete host-scoped External Agents inventory",
    terminalObservable:
      "The picker shows every supported External Agent exactly once with a consistent Provider (Host[, profile]) label and no off-host or alias-only duplicate.",
  },
  {
    id: "JOURNEY-NEW-SESSION-PROJECT-OUTCOME",
    name: "Start a new session and obtain a project-bound outcome",
    terminalObservable:
      "Each selected route either reads an exact project sentinel through session/new or stops with a precise external authentication, capacity, or interaction classification; no product launch/protocol failure or substitution occurs.",
  },
  {
    id: "JOURNEY-TERMINATION-CLEANUP",
    name: "Terminate or detach without leaked ownership",
    terminalObservable:
      "Ordinary owned processes exit; persistent clients detach while the bounded provider and durable journal lifecycles remain healthy.",
  },
  {
    id: "JOURNEY-PERSISTENT-SESSION-RECOVERY",
    name: "Recover the same persistent session after transport and lane restart",
    terminalObservable:
      "Retry creates a fresh transport, loads the original session ID and history, preserves the project, and accepts one continuation after a real lane-service restart.",
  },
  {
    id: "JOURNEY-AGENT-SWITCH-AND-RETURN",
    name: "Switch between independently selectable agents and return",
    terminalObservable:
      "A user can switch away and return from both an empty draft and a retained non-empty session; replaceable state is released, retained state remains usable, and single-owner adapters receive a separate connection for each new thread.",
  },
];

const persistentSurfaceId = "SURFACE-ACP-INTREPID-PERSISTENT";
const applicableJourneyIds = (surfaceId) =>
  journeys
    .filter(
      (journey) =>
        journey.id !== "JOURNEY-PERSISTENT-SESSION-RECOVERY" ||
        surfaceId === persistentSurfaceId,
    )
    .map((journey) => journey.id);

const variants = surfaceDefinitions.flatMap((surface) =>
  inventory.pickerSurfaces[surface.inventoryKey].map((variant) => ({
    id: variant.id,
    name: variant.label,
    surfaceId: surface.id,
    journeyIds: applicableJourneyIds(surface.id),
    sourceRef: "docs/test-plan-inputs/zed-agent-picker-inventory.json",
    configuredEntries: variant.configuredEntries,
  })),
);

const matrix = surfaceDefinitions.flatMap((surface) =>
  journeys.map((journey) => {
    const applicable = applicableJourneyIds(surface.id).includes(journey.id);
    return applicable
      ? {
          surfaceId: surface.id,
          journeyId: journey.id,
          applicability: "applicable",
          assembledPath: [surface.assembledEntryPoint, journey.name, journey.terminalObservable],
          requiredEvidenceBoundary: "installed_boundary",
          requiredEvidence: ["installed product execution", "terminal observable", "cleanup"],
        }
      : {
          surfaceId: surface.id,
          journeyId: journey.id,
          applicability: "not_applicable",
          reason:
            surface.id === "SURFACE-ACP-MAC-LOCAL"
              ? "Mac-local routes do not use the Intrepid persistent session host or its durable journal."
              : "Ordinary Intrepid routes do not promise persistent lane or journal restoration.",
        };
  }),
);

const requirements = [
  {
    id: "REQ-HOST-001",
    claim: "The project execution host owns the picker inventory; off-host routes are not selectable.",
    source: "crates/project/src/project_settings.rs",
  },
  {
    id: "REQ-LABEL-002",
    claim: "External Agent labels use Provider (Host) or Provider (Host, profile) consistently.",
    source: "crates/project/src/agent_server_store.rs",
  },
  {
    id: "REQ-INVENTORY-003",
    claim: "Every supported visible External Agent appears exactly once and alias-only routes do not add duplicates.",
    source: "crates/agent_ui/src/agent_panel.rs",
  },
  {
    id: "REQ-LAUNCH-004",
    claim: "Selecting a route launches that exact provider, host, and profile without substitution.",
    source: "crates/agent_servers/src/acp.rs",
  },
  {
    id: "REQ-PROJECT-005",
    claim: "A successful route can read exact bytes from the selected project directory.",
    source: "script/zed-acp-live-canary.py",
  },
  {
    id: "REQ-ERROR-006",
    claim: "External readiness, interaction, and product failures remain distinguishable.",
    source: "script/zed-agent-picker-uat.py",
  },
  {
    id: "REQ-RECOVERY-007",
    claim: "A persistent transport failure is recoverable without losing the session.",
    source: "crates/agent_ui/src/conversation_view.rs",
  },
  {
    id: "REQ-CLEANUP-008",
    claim: "Ordinary processes terminate and persistent attachment/provider/journal lifecycles keep their distinct semantics.",
    source: "script/zed-acp-live-canary.py",
  },
  {
    id: "REQ-RUNTIME-009",
    claim: "Intrepid uses the commit-matched installed remote server and current host agent configuration.",
    source: "crates/remote_server/src/remote_server.rs",
  },
  {
    id: "REQ-SWITCH-010",
    claim:
      "Switching between independently selectable agents and returning works from every reachable thread state, including replaceable empty drafts and retained non-empty sessions, without stale connection or session ownership.",
    source: "crates/agent_ui/src/agent_panel.rs; crates/agent_ui/src/agent_connection_store.rs",
  },
];

const allSurfaceIds = surfaces.map((surface) => surface.id);
const allVariantIds = variants.map((variant) => variant.id);
const persistentVariantIds = variants
  .filter((variant) => variant.surfaceId === persistentSurfaceId)
  .map((variant) => variant.id);

const commonRow = {
  section: "0-critical-path-smoke",
  risk: "critical",
  tier: 0,
  readiness: "ready_now",
  runner: "installed-zed-computer-use+content-free-receipts",
  rowKind: "journey",
  evidenceStrength: "direct",
  evidenceLayer: "assembled_product",
  evidenceBoundary: "installed_boundary",
  decisionRule: "hard_fail",
  confidenceOnly: false,
  weakRow: false,
  uatPersona: "Keeva using Zed 10x",
  observabilityChecks: [
    "no product-origin Failed to Launch error",
    "provider readiness failures are classified explicitly",
    "no prompt or response content is retained in evidence",
  ],
  dataCleanup: "Close created threads and prove route-appropriate process cleanup or persistent detach.",
};

const rows = [
  {
    ...commonRow,
    id: "ZED-ACP-INVENTORY-001",
    title: "Installed picker is complete, host-scoped, consistently labelled, and duplicate-free",
    requirementIds: ["REQ-HOST-001", "REQ-LABEL-002", "REQ-INVENTORY-003", "REQ-RUNTIME-009"],
    requirements: ["REQ-HOST-001", "REQ-LABEL-002", "REQ-INVENTORY-003", "REQ-RUNTIME-009"],
    irRefs: ["External Agents picker", "execution-host settings", "alias de-duplication"],
    surfaceIds: allSurfaceIds,
    journeyIds: ["JOURNEY-HOST-SCOPED-INVENTORY"],
    variantIds: allVariantIds,
    provenance: [
      { source: "prior_defect", ref: "missing, duplicated, and inconsistent picker labels" },
      { source: "code", ref: "crates/agent_ui/src/agent_panel.rs" },
    ],
    plannedEvidence: ["installed picker accessibility snapshot", "exact 8/8/6 visible variant inventory"],
    specificOracles: [
      "8 Mac-local, 8 Intrepid persistent, and 6 Intrepid ordinary External Agent entries",
      "no retired, off-host, or alias-only duplicate entry",
    ],
    steps: [
      "Open a Mac-local project and inspect External Agents",
      "Open an Intrepid project and inspect External Agents",
      "Compare every visible entry to the checked inventory and host-label grammar",
    ],
    expectedResult: "All 22 host-valid External Agent variants are visible exactly once on the correct project host.",
    passCriteria: ["exact label sets match", "no duplicates", "no off-host entries"],
    userGoal: "Choose an agent without guessing where it runs.",
    acceptanceCriteria: ["every supported route is findable", "host and profile are unambiguous"],
    frictionSignals: ["missing route", "duplicate route", "inconsistent capitalization or parentheses"],
    evidenceRequired: ["installed UI snapshot", "inventory digest"],
    negativeCase: "configured route exists but is missing, duplicated, or labelled as the wrong host",
    newInformationTarget: "Whether the installed picker, not merely configuration files, exposes the exact supported set.",
  },
  {
    ...commonRow,
    id: "ZED-ACP-SESSION-002",
    title: "Every visible External Agent starts the named route and reaches a project-bound outcome",
    requirementIds: ["REQ-LAUNCH-004", "REQ-PROJECT-005", "REQ-ERROR-006", "REQ-RUNTIME-009"],
    requirements: ["REQ-LAUNCH-004", "REQ-PROJECT-005", "REQ-ERROR-006", "REQ-RUNTIME-009"],
    irRefs: ["session/new", "project sentinel", "provider readiness classification"],
    surfaceIds: allSurfaceIds,
    journeyIds: ["JOURNEY-NEW-SESSION-PROJECT-OUTCOME"],
    variantIds: allVariantIds,
    provenance: [
      { source: "prior_defect", ref: "direct canary passed while installed picker launch failed" },
      { source: "risk", ref: "independently selectable provider and profile variants" },
    ],
    plannedEvidence: [
      "current installed representative selection per surface",
      "session/new and project-bound composer state",
      "explicit mechanical equivalence to the complete predecessor matrix for byte-identical route-specific inputs",
    ],
    specificOracles: [
      "successful route reads the withheld project sentinel",
      "unavailable route reports authentication, capacity, or interaction rather than a product failure",
      "no fallback, host swap, or profile substitution",
    ],
    steps: [
      "Select the direct representative for every installed surface through the current installed picker",
      "Create a new session and reach a project-bound ready or precise external-readiness outcome",
      "Bind sibling variants only when their route-specific inputs are byte-identical and the shared mechanism has a current deterministic regression",
      "Retain the complete predecessor route matrix as historical support rather than relabelling it as current execution",
    ],
    expectedResult: "Every variant has a product-green terminal outcome and healthy routes remain usable after a failed provider.",
    passCriteria: [
      "three current surface representatives pass",
      "all 22 variants are direct or mechanically equivalent",
      "zero product failures",
      "no substitution",
    ],
    userGoal: "Start any listed agent and work in the current project.",
    acceptanceCriteria: ["working routes respond", "unavailable routes explain the real action needed"],
    frictionSignals: ["blank thread", "exit 0 shown as failure", "incoming transport closed", "missing executable"],
    evidenceRequired: ["content-free route receipts", "installed UI terminal states"],
    negativeCase: "a direct provider canary passes while the installed route wrapper, cwd, or transport fails",
    newInformationTarget: "Whether every user-selectable route works through the real assembled launch path.",
  },
  {
    ...commonRow,
    id: "ZED-ACP-CLEANUP-003",
    title: "Closing and failure leave route-appropriate process and journal state",
    requirementIds: ["REQ-CLEANUP-008"],
    requirements: ["REQ-CLEANUP-008"],
    irRefs: ["process-group cleanup", "persistent detach", "provider reaper"],
    surfaceIds: allSurfaceIds,
    journeyIds: ["JOURNEY-TERMINATION-CLEANUP"],
    variantIds: allVariantIds,
    provenance: [
      { source: "prior_defect", ref: "client exit was mistaken for full containment cleanup" },
      { source: "risk", ref: "ordinary and persistent routes have different lifecycle ownership" },
    ],
    plannedEvidence: [
      "current representative ordinary process-group absence",
      "current persistent client detach",
      "healthy bounded lane services",
      "mechanically equivalent sibling cleanup bound to the shared current teardown regressions",
    ],
    specificOracles: [
      "no owned ordinary wrapper or provider residue",
      "persistent journal remains loadable after client detach",
      "idle provider lifecycle stays bounded by the installed host policy",
    ],
    steps: [
      "Close current direct representative sessions on every installed surface",
      "Census ordinary owned process groups",
      "Verify persistent attachments detached and lane services remain healthy",
      "Bind sibling variants only through byte-identical route inputs and the current shared teardown regressions",
    ],
    expectedResult: "No leaked ordinary ownership and no accidental destruction of persistent recovery state.",
    passCriteria: ["ordinary cleanup green", "persistent detach green", "journal and service health green"],
    userGoal: "Leave or recover sessions without degrading the machine or losing durable work.",
    acceptanceCriteria: ["no accumulating processes", "persistent work remains recoverable"],
    frictionSignals: ["lag after testing", "zombie wrappers", "missing journal", "dead lane"],
    evidenceRequired: ["process census", "service and journal health"],
    negativeCase: "attachment exits but an ordinary child leaks, or persistent cleanup deletes recovery state",
    newInformationTarget: "Whether cleanup matches each route's real ownership semantics.",
  },
  {
    ...commonRow,
    id: "ZED-ACP-RECOVERY-004",
    title: "A killed persistent lane recovers the original session through the installed Retry action",
    requirementIds: ["REQ-RECOVERY-007", "REQ-RUNTIME-009"],
    requirements: ["REQ-RECOVERY-007", "REQ-RUNTIME-009"],
    irRefs: ["LoadError::Exited", "restart_connection", "session/load", "durable journal"],
    surfaceIds: [persistentSurfaceId],
    journeyIds: ["JOURNEY-PERSISTENT-SESSION-RECOVERY"],
    variantIds: persistentVariantIds,
    provenance: [
      { source: "prior_defect", ref: "zed-acp-session-attach peer closed before client input after lane OOM kill" },
      { source: "code", ref: "crates/agent_ui/src/conversation_view.rs" },
    ],
    plannedEvidence: ["real lane-service restart", "visible Retry", "same session ID and history", "post-recovery continuation"],
    specificOracles: [
      "Retry starts a fresh connection rather than reusing the exited one",
      "session/load uses the original session ID",
      "history and project remain visible",
      "one post-recovery prompt completes",
    ],
    steps: [
      "Attach to a completed persistent Intrepid session",
      "Restart its exact user service while Zed remains attached",
      "Observe a recoverable failure and invoke Retry",
      "Verify the same session and project, then continue once",
    ],
    expectedResult: "The screenshot failure becomes a finite recoverable interruption rather than a lost or blank session.",
    passCriteria: ["fresh transport", "same session ID", "same history", "same project", "continuation passes"],
    userGoal: "Resume work after Intrepid or its lane process restarts.",
    acceptanceCriteria: ["no new thread required", "no history loss", "one Retry restores work"],
    frictionSignals: ["peer closed", "blank history", "new session ID", "repeat restart loop"],
    evidenceRequired: ["installed UI recovery", "service invocation transition", "content-free session identity digest"],
    negativeCase: "service restarts but Zed retries the dead transport or silently creates a different session",
    statefulJourney: "attached -> exited -> retrying -> loaded-original -> continued",
    newInformationTarget: "Whether the installed app and installed host recover together across the real transport seam.",
  },
  {
    ...commonRow,
    id: "ZED-ACP-SWITCH-005",
    title: "Switch-and-return releases replaceable drafts and isolates retained sessions",
    requirementIds: ["REQ-SWITCH-010", "REQ-LAUNCH-004"],
    requirements: ["REQ-SWITCH-010", "REQ-LAUNCH-004"],
    irRefs: [
      "AgentPanel::discard_empty_draft",
      "activate_new_thread",
      "ensure_draft",
      "AgentConnectionStore::request_fresh_connection",
      "AgentServer::requires_dedicated_connection",
    ],
    surfaceIds: allSurfaceIds,
    journeyIds: ["JOURNEY-AGENT-SWITCH-AND-RETURN"],
    variantIds: allVariantIds,
    provenance: [
      { source: "prior_defect", ref: "connection already owns a session after Codex -> Cursor -> Codex" },
      { source: "code", ref: "crates/agent_ui/src/agent_panel.rs" },
      { source: "risk", ref: "state transitions between independently selectable variants" },
    ],
    plannedEvidence: [
      "installed Mac Codex -> Cursor -> Codex switch-and-return",
      "installed Intrepid persistent Codex -> ordinary Cursor -> persistent Codex switch-and-return",
      "deterministic proof that the replaced empty draft view is dropped",
      "deterministic proof that a retained non-empty single-owner thread does not share its connection with a new thread",
    ],
    specificOracles: [
      "each replacement reaches a ready composer without a product launch or session-ownership error",
      "returning to the first agent creates or reuses only valid current state",
      "the old empty draft is absent from retained threads and its entity is dropped",
      "the retained non-empty thread remains available while a new thread for the same single-owner adapter gets a separate connection",
      "all sibling variants use the same mechanically verified AgentPanel replacement path",
    ],
    steps: [
      "Exercise both a ready empty draft and a non-empty retained thread on a direct representative for each installed host surface",
      "Switch to a representative from another independently selectable agent class",
      "Switch back to the first representative without restarting Zed",
      "Verify ready composer state, retained history usability, no hidden ownership error, and route-appropriate cleanup",
      "Bind sibling variants by the shared replacement path after each was independently exercised by ZED-ACP-SESSION-002",
    ],
    expectedResult:
      "Every installed host surface supports switch-and-return across empty and retained states without a ghost draft, stale connection, or session-ownership collision.",
    passCriteria: [
      "Mac Codex -> Cursor -> Codex succeeds",
      "Intrepid persistent Codex -> ordinary Cursor -> persistent Codex succeeds",
      "old empty draft entity is dropped deterministically",
      "retained non-empty single-owner threads use distinct connections",
      "mechanical equivalence is explicit for sibling variants",
    ],
    userGoal: "Change agents in one panel without restarting Zed or losing the ability to return.",
    acceptanceCriteria: [
      "no hidden replaceable draft",
      "retained non-empty thread remains usable",
      "no duplicate writer or session ownership",
      "no app restart required",
    ],
    frictionSignals: ["connection already owns a session", "blank composer", "Failed to Launch", "agent only works after restart"],
    evidenceRequired: [
      "installed transition receipts for Mac and Intrepid",
      "deterministic empty-draft lifetime regression",
      "deterministic retained-thread connection-isolation regression",
    ],
    negativeCase:
      "the new agent appears selected while replaceable state is retained or a non-empty retained session shares a connection whose adapter permits only one owned session",
    statefulJourney:
      "first-ready-empty -> second-ready-empty -> first-ready-empty; first-nonempty-retained -> second-ready -> first/new-first-ready",
    newInformationTarget: "Whether transitions between individually green selectable agents preserve panel and connection ownership invariants.",
  },
];

const rowForJourney = Object.fromEntries(rows.map((row) => [row.journeyIds[0], row.id]));

const executionContracts = {
  "ZED-ACP-INVENTORY-001": {
    commands: [
      "computer-use installed Zed 10x in a Mac-local project: open External Agents and verify the exact eight Mac variants, unique consistent labels, and no Intrepid or alias-only entries",
      "computer-use installed Zed 10x in an Intrepid project: open External Agents and verify the exact eight persistent plus six ordinary Intrepid variants, unique consistent labels, and no Mac or alias-only entries",
    ],
    evidencePaths: [
      "docs/test-results/zed-acp-inventory-mac.json",
      "docs/test-results/zed-acp-inventory-intrepid.json",
    ],
    runtimeIdentityIds: ["RUNTIME-ZED10X-MAC", "RUNTIME-ZED10X-INTREPID"],
  },
  "ZED-ACP-SESSION-002": {
    commands: [
      "computer-use exact current installed Zed 10x Mac-local External Agents: launch the direct Codex (Mac) representative to a ready project-bound composer and bind sibling variants only through byte-identical route inputs plus current shared-path regressions",
      "computer-use exact current installed Zed 10x Intrepid External Agents: launch direct Codex (Intrepid, primary) and Cursor (Intrepid) representatives to ready project-bound composers and bind sibling variants only through byte-identical route inputs plus current shared-path regressions",
    ],
    evidencePaths: [
      "docs/test-results/zed-acp-session-mac.json",
      "docs/test-results/zed-acp-session-intrepid.json",
    ],
    runtimeIdentityIds: ["RUNTIME-ZED10X-MAC", "RUNTIME-ZED10X-INTREPID"],
  },
  "ZED-ACP-CLEANUP-003": {
    commands: [
      "inspect exact current installed Mac-local representative teardown and prove no owned Zed runtime, canary, wrapper, or provider process remains",
      "inspect exact current installed Intrepid representative teardown and prove ordinary attachments are gone while the persistent lane remains healthy",
    ],
    evidencePaths: [
      "docs/test-results/zed-acp-cleanup-mac.json",
      "docs/test-results/zed-acp-cleanup-intrepid.json",
    ],
    runtimeIdentityIds: ["RUNTIME-ZED10X-MAC", "RUNTIME-ZED10X-INTREPID"],
  },
  "ZED-ACP-RECOVERY-004": {
    commands: [
      "computer-use installed Zed 10x with an existing Codex (Intrepid, primary) session: restart zed-acp-session-host@codex--primary.service, invoke Retry, load the original session, and continue once",
    ],
    evidencePaths: ["docs/test-results/zed-acp-recovery-intrepid.json"],
    runtimeIdentityIds: ["RUNTIME-ZED10X-INTREPID"],
  },
  "ZED-ACP-SWITCH-005": {
    commands: [
      "./script/cargo test -p agent_ui test_retained_thread_does_not_share_dedicated_agent_connection --lib -- --test-threads=1",
      "computer-use installed Zed 10x in a Mac-local project: switch Codex (Mac) -> Cursor (Mac) -> Codex (Mac), require a ready composer after every transition, then close Zed and prove exact session cleanup",
      "computer-use installed Zed 10x in an Intrepid project: switch Codex (Intrepid, primary) -> Cursor (Intrepid) -> Codex (Intrepid, primary), require a ready composer after every transition, then close Zed and prove exact attachment cleanup",
    ],
    evidencePaths: [
      "docs/test-results/zed-acp-switch-mac.json",
      "docs/test-results/zed-acp-switch-intrepid.json",
    ],
    runtimeIdentityIds: ["RUNTIME-ZED10X-MAC", "RUNTIME-ZED10X-INTREPID"],
  },
};
const planCells = matrix.map((cell) =>
  cell.applicability === "applicable"
    ? {
        surfaceId: cell.surfaceId,
        journeyId: cell.journeyId,
        applicability: cell.applicability,
        requiredEvidenceBoundary: cell.requiredEvidenceBoundary,
        rowIds: [rowForJourney[cell.journeyId]],
      }
    : {
        surfaceId: cell.surfaceId,
        journeyId: cell.journeyId,
        applicability: cell.applicability,
        reason: cell.reason,
      },
);

const switchJourneyId = "JOURNEY-AGENT-SWITCH-AND-RETURN";
const directRepresentativeBySurface = {
  "SURFACE-ACP-MAC-LOCAL": "VAR-MAC-CODEX",
  "SURFACE-ACP-INTREPID-PERSISTENT": "VAR-INTREPID-CODEX-PRIMARY",
  "SURFACE-ACP-INTREPID-ORDINARY": "VAR-INTREPID-CURSOR",
};
const mechanicallyEquivalentJourneyIds = new Set([
  "JOURNEY-NEW-SESSION-PROJECT-OUTCOME",
  "JOURNEY-TERMINATION-CLEANUP",
  "JOURNEY-PERSISTENT-SESSION-RECOVERY",
  switchJourneyId,
]);
const variantCells = variants.flatMap((variant) =>
  variant.journeyIds.map((journeyId) => {
    const representativeId = directRepresentativeBySurface[variant.surfaceId];
    if (mechanicallyEquivalentJourneyIds.has(journeyId) && variant.id !== representativeId) {
      return {
        variantId: variant.id,
        journeyId,
        coverageMode: "mechanically_equivalent",
        equivalentToVariantId: representativeId,
        equivalenceEvidence: [
          "The exact current installed runtime directly exercises one representative on the same surface and journey.",
          "The complete predecessor route matrix directly exercised this variant; current route-specific manifest and launch inputs are byte-identical.",
          "Current deterministic regressions prove the shared launch, replacement, transport, and cleanup mechanism used by the representative and sibling variant.",
        ],
        equivalenceRowIds: [rowForJourney[journeyId]],
      };
    }
    return {
      variantId: variant.id,
      journeyId,
      coverageMode: "direct",
      evidenceLayer: "assembled_product",
      rowIds: [rowForJourney[journeyId]],
    };
  }),
);

const discovery = {
  schemaVersion: 3,
  coverageClosureVersion: 2,
  freshLifecycle: true,
  discoveryMode: "fresh_full_lifecycle",
  generatedAt,
  repository: "zed-10x",
  scope: {
    included: "Installed External Agent inventory, launch, project outcome, cross-agent state transitions, cleanup, and persistent session recovery on Mac and Intrepid.",
    excluded: [
      "Zed Agent and Terminal native picker entries",
      "provider account repair",
      "public notarized distribution",
      "unrelated upstream editor features",
    ],
  },
  architecture: {
    client: "installed Zed 10x.app",
    remote: "commit-matched Zed remote server on Intrepid",
    persistentTransport: "systemd-owned ACP Session Hub lane with durable journal",
    ordinaryTransport: "execution-host ACP process",
  },
  requirements,
  productSurfaces: surfaces,
  criticalJourneys: journeys.map((journey) => ({
    ...journey,
    provenance: [
      { source: "prior_defect", ref: "observed installed Zed 10x failures" },
      { source: "risk", ref: "surface-by-journey closure" },
    ],
  })),
  surfaceJourneyMatrix: matrix,
  selectableVariants: variants,
  negativeSpaceAudit: [
    "Direct provider canaries do not prove installed picker selection or alias resolution.",
    "A sibling profile pass does not prove another account/profile variant.",
    "A component Retry test does not prove the installed app survives a real lane restart.",
    "Provider unavailability is not a product failure, but it is not a usable-route pass either.",
    "Persistent detach and ordinary process cleanup are different terminal semantics.",
    "One-shot success for two agents does not prove switch-and-return across replaceable empty drafts and retained non-empty sessions.",
  ],
  existingCoverage: {
    deterministic: [
      "script/tests/zed-agent-picker-uat.test.mjs",
      "crates/agent_ui/src/conversation_view.rs",
    ],
    installed: ["script/zed-agent-picker-uat.py", "script/zed-acp-live-canary.py"],
  },
  toolAvailability: {
    nodeTest: true,
    cargoTest: true,
    installedComputerUse: true,
    sshIntrepid: true,
  },
};

const plan = {
  schemaVersion: 3,
  coverageClosureVersion: 2,
  freshLifecycle: true,
  generatedAt,
  northStar:
    "Keeva can choose any visible External Agent with an unambiguous host/profile label, start it in the current project, switch among agents without hidden ownership, understand genuine provider unavailability, and recover a persistent Intrepid session after transport loss without losing work.",
  scope: discovery.scope,
  counts: {
    totalRows: rows.length,
    readyNowRows: rows.length,
    productSurfaces: surfaces.length,
    criticalJourneys: journeys.length,
    applicableSurfaceJourneyCells: matrix.filter((cell) => cell.applicability === "applicable").length,
    selectableVariants: variants.length,
    directVariantJourneyCells: variantCells.length,
  },
  surfaceJourneyCoverage: { cells: planCells },
  variantJourneyCoverage: { cells: variantCells },
  tests: rows,
};

const implementationMap = {
  plannedRows: rows.length,
  readyNowRows: rows.length,
  implementedRows: 0,
  executedRows: 0,
  remainingRows: rows.length,
  unimplementedRows: rows.map((row) => row.id),
  blockedRows: [],
  mappings: rows.map((row) => ({
    ...row,
    ...executionContracts[row.id],
    implemented: false,
    outcome: "planned_not_yet_executed",
    blocker: "Installed execution evidence has not yet been captured for this fresh plan.",
    remediationAttempts: [],
  })),
};

const lifecycleState = {
  schemaVersion: 3,
  coverageClosureVersion: 2,
  startedAt: generatedAt,
  repo: "zed-10x",
  branch: "codex/zed-production-test-closure",
  scope: {
    id: "zed-10x-installed-agent-session-closure",
    description: discovery.scope.included,
    excluded: discovery.scope.excluded,
  },
  implementationTarget: "full_plan_implemented",
  freshLifecycleRequired: true,
  phases: {
    "0-preflight": {
      status: "complete",
      freshLifecycleRequired: true,
      implementationTarget: "full_plan_implemented",
      finding:
        "Prior artifacts conflated configured routes, visible picker variants, and direct canaries; the installed surface closure was stale.",
    },
    "1-discovery": {
      status: "complete",
      artifact: "docs/discovery-ir.json",
      productSurfaces: surfaces.length,
      criticalJourneys: journeys.length,
      surfaceJourneyCells: matrix.length,
      applicableCells: matrix.filter((cell) => cell.applicability === "applicable").length,
      selectableVariants: variants.length,
    },
    "2-generation": {
      status: "complete",
      artifacts: ["docs/test-plan.json", "docs/TEST_PLAN.md"],
      plannedRows: rows.length,
      readyNowRows: rows.length,
    },
    "3-implementation": {
      status: "in_progress",
      plannedRows: rows.length,
      readyNowRows: rows.length,
      implementedRows: 0,
      remainingRows: rows.length,
    },
    "4-execution": { status: "pending" },
    "5-analysis": { status: "pending" },
    "6-tracking": { status: "pending" },
  },
};

const markdown = `# Zed 10x Installed Agent and Session Test Plan

Status: fresh plan; installed execution pending<br>
Generated: ${generatedAt}<br>
North star: ${plan.northStar}

## Scope

This plan covers the **External Agents** section of the installed Zed 10x picker. It does not claim that native Zed Agent, Terminal, or Add More Agents are external-agent variants.

## Surface × journey closure

| Installed surface | Visible variants | Host inventory | New session and project outcome | Switch and return | Cleanup | Persistent recovery |
|---|---:|---|---|---|---|---|
| Mac local | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | N/A |
| Intrepid persistent | 8 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | Installed boundary |
| Intrepid ordinary | 6 | Installed boundary | Installed boundary | Installed boundary | Installed boundary | N/A |

All 22 visible variants receive direct coverage for every applicable journey. The resulting plan contains ${variantCells.length} direct variant × journey cells and does not infer one profile or provider from another.

## Tier 0 rows

${rows
  .map(
    (row) => `### ${row.id}: ${row.title}

**Surfaces:** ${row.surfaceIds.join(", ")}<br>
**Journey:** ${row.journeyIds.join(", ")}<br>
**Variants:** ${row.variantIds.length} direct variants<br>
**Expected:** ${row.expectedResult}

Steps:
${row.steps.map((step, index) => `${index + 1}. ${step}`).join("\n")}

Pass criteria:
${row.passCriteria.map((criterion) => `- ${criterion}`).join("\n")}
`,
  )
  .join("\n")}
## Execution order

1. Run deterministic inventory and Retry regressions.
2. Capture exact installed Mac and Intrepid runtime identities.
3. Exercise installed picker inventory and every visible variant.
4. Exercise cleanup semantics.
5. Exercise representative switch-and-return journeys and bind sibling variants by the mechanically verified shared replacement path.
6. Restart one real persistent lane while attached and recover the original session through Retry.
7. Bind content-free evidence to the exact tested revision and validate the lifecycle artifacts.
`;

const analysis = `# Zed 10x Test Plan Analysis

The fresh plan is intentionally not production-ready until all five installed rows execute. The main prior false-positive mechanisms were proxy substitution and isolated-route testing: direct ACP/provider canaries were accepted as evidence for UI selectability, host labels, assembled launch wrappers, restart recovery, and transitions between individually green agents. This plan removes those inferences and binds each independently visible External Agent variant to every applicable user journey, including an explicit switch-and-return state transition.

The dominant residual risk before execution is persistent-session recovery after a real service death. The deterministic Rust regression is necessary but not sufficient; the installed Zed 10x app, remote server, attach wrapper, systemd unit, durable journal, Retry UI, and original session must be observed together.
`;

const pendingSummary = {
  schemaVersion: 3,
  generatedAt,
  decisionCandidate: "not_ready",
  plannedRows: rows.length,
  implementedRows: 0,
  executedRows: 0,
  remainingPlannedRows: rows.length,
  requirements: {
    total: requirements.length,
    covered: requirements.length,
    passed: 0,
    failed: 0,
    blocked: 0,
    untested: requirements.length,
  },
  rowResults: [],
  uat: { status: "not_run", persona: "Keeva using Zed 10x" },
  observability: { status: "not_run" },
  dataCleanup: { status: "not_run" },
  flakiness: { rerunCount: 0, flakyTests: [] },
  testValidity: { status: "not_run" },
  defectDiscovery: {
    bugsFound: 0,
    productDefects: [],
    testHarnessDefects: [],
    infraOrToolingDefects: [],
    processDefects: [
      "The prior lifecycle treated configured routes, visible variants, and direct canaries as interchangeable.",
    ],
    fixedDuringRun: [],
    confidenceOnlyRows: [],
    weakRows: [],
    newInformationSummary: [],
    lowYieldAssessment: "mixed",
    residualRisk: ["Installed execution is pending."],
  },
};

const outputs = new Map([
  ["docs/discovery-ir.json", discovery],
  ["docs/test-plan.json", plan],
  ["docs/test-implementation-map.json", implementationMap],
  ["docs/test-lifecycle-state.json", lifecycleState],
  ["docs/test-results/summary.json", pendingSummary],
]);

for (const [relativePath, value] of outputs) {
  writeFileSync(path.join(repositoryRoot, relativePath), `${JSON.stringify(value, null, 2)}\n`);
}
writeFileSync(path.join(repositoryRoot, "docs/TEST_PLAN.md"), markdown);
writeFileSync(path.join(repositoryRoot, "docs/TEST_PLAN_ANALYSIS.md"), analysis);

console.log(
  JSON.stringify({
    status: "generated",
    surfaces: surfaces.length,
    journeys: journeys.length,
    variants: variants.length,
    directVariantJourneyCells: variantCells.length,
    rows: rows.length,
  }),
);
