/**
 * SPDX-License-Identifier: AGPL-3.0-only
 *
 * OIS-origin tooling for the stage-4b acceptance corpus (see CONVERGENCE.md).
 *
 * Stage-4b TypeScript differential driver (reproduction of the scratch-only
 * stage-3/4a driver, pinned to second-brain @ frontier/ois-v1 c56f8e5).
 *
 * Runs the parity corpus through the pinned TypeScript oracle implementation:
 *  - engine cases (94): the pinned packages/core, packages/work, and
 *    packages/projections engines, projected 1:1 into the Rust harness's
 *    normalized shapes;
 *  - fixture cases (41): the pinned testkit reference (echo) engine over the
 *    fixture catalog, projected onto each fixture's partition-bound step
 *    fields (the oracle's assertion surface).
 *
 * Usage:
 *   tsx run_ts.ts <parity-corpus.json> <ts-results.json>
 *
 * Oracle root defaults to /home/user/work/oracle-scratch (override with
 * ORACLE_ROOT). The driver is read-only with respect to the oracle checkout.
 */

import * as fs from "node:fs";

type Json = Record<string, any>;

const ORACLE_ROOT: string = process.env.ORACLE_ROOT ?? "/home/user/work/oracle-scratch";

// ---------------------------------------------------------------------------
// Pinned oracle imports (dynamic: absolute paths into the read-only checkout,
// resolved by the oracle's own node_modules).
// ---------------------------------------------------------------------------

const { canonicalDigest } = await import(
  ORACLE_ROOT + "/packages/core/src/canonical.ts"
);
const { resolveCurrentState } = await import(
  ORACLE_ROOT + "/packages/core/src/resolver/resolve.ts"
);
const { resolveCriterion } = await import(
  ORACLE_ROOT + "/packages/core/src/resolver/criterion.ts"
);
const { applyGovernedWrite, acceptCrossScope, isStaleBase } =
  await import(ORACLE_ROOT + "/packages/core/src/governance/write.ts");
const { evaluateCapability: evaluateCapabilityBase } = await import(
  ORACLE_ROOT + "/packages/core/src/governance/profile.ts"
);
const { classifyChangeClass } = await import(
  ORACLE_ROOT + "/packages/core/src/governance/events.ts"
);
const { promoteProjectionEdges } = await import(
  ORACLE_ROOT + "/packages/work/src/promotion.ts"
);
const { churnKeyRecordedAt } = await import(
  ORACLE_ROOT + "/packages/projections/src/change-dynamics/model.ts"
);
const { echoEngine, runFixture, loadCatalog, loadGrCatalog, partitionFor } =
  await import(ORACLE_ROOT + "/packages/testkit/src/index.ts");

const corpus: Json = JSON.parse(fs.readFileSync(process.argv[2]!, "utf8"));
const outPath: string = process.argv[3]!;

// ---------------------------------------------------------------------------
// Small shared helpers (mirror the Rust harness's normalization exactly).
// ---------------------------------------------------------------------------

const opt = (value: unknown): unknown => (value === undefined ? null : value);

function epochSeconds(iso: string): number {
  const [date, time = "00:00:00"] = iso.split("T");
  const [y, m, d] = date.split("-").map(Number);
  // Days-from-civil (Hinnant) — same algorithm as the Rust harness.
  const yAdj = m! <= 2 ? y! - 1 : y!;
  const era = Math.floor(yAdj / 400);
  const yoe = yAdj - era * 400;
  const mp = m! > 2 ? m! - 3 : m! + 9;
  const doy = Math.floor((153 * mp + 2) / 5) + d! - 1;
  const doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy;
  const days = era * 146097 + doe - 719468;
  const [hh, mm, rest = ""] = time.split(":");
  const [ss] = rest.split(".");
  return days * 86400 + Number(hh) * 3600 + Number(mm) * 60 + Number(ss);
}

function inInclusiveInterval(instant: string, start: string, end: string): boolean {
  return instant >= start && instant <= end;
}

function scopesOf(input: Json): string[] {
  return Array.isArray(input.target_scope_refs) ? input.target_scope_refs : [];
}

// ---------------------------------------------------------------------------
// resolver — mirrors parity.rs run_resolver + project_resolver.
// ---------------------------------------------------------------------------

function projectResolver(c: Json): Json {
  const out: Json = { state: c.state };
  if (c.completeness !== undefined) out.completeness = c.completeness;
  out.diagnostic_kinds = (c.diagnostics ?? []).map((d: Json) => d.kind);
  out.envelope_recorded_at = opt(c.envelope?.recorded_at);
  if (c.basis !== undefined) out.basis = c.basis;
  if (c.authority !== undefined) out.authority = c.authority;
  if (c.state === "superseded") out.successor_ref = opt(c.successor_ref ?? c.successorRef);
  if (c.state === "permission_limited") {
    const withheld = (c.diagnostics ?? []).find((d: Json) => d.kind === "permission_withheld");
    out.withheld_count = opt(withheld?.withheld_count);
  }
  return out;
}

function runResolver(input: Json): Json {
  const corpusA: Json = input.corpus ?? input.corpus_a;
  const query: Json = input.query;
  const resolveEach = input.resolve_each !== undefined;

  const combined: Json = resolveEach
    ? { state: "not_established" }
    : resolveCurrentState(corpusA, query);
  const a = projectResolver(combined);
  const result: Json = { ...a };

  if (input.corpus_b !== undefined) {
    const b = projectResolver(
      resolveCurrentState(input.corpus_b, input.query_b ?? query) as Json,
    );
    if (input.assert_truncated_equivalence !== undefined) {
      result.equal_to_b = JSON.stringify(a) === JSON.stringify(b);
    }
    result.state_b = b.state;
  }

  if (input.query_b !== undefined && input.corpus_b === undefined) {
    const b = projectResolver(resolveCurrentState(corpusA, input.query_b) as Json);
    result.state_b = b.state;
  }

  if (input.also_run_shuffled !== undefined) {
    const history = [...corpusA.target_history].reverse();
    const shuffled = projectResolver(
      resolveCurrentState({ ...corpusA, target_history: history }, query) as Json,
    );
    result.shuffled_equal = JSON.stringify(a) === JSON.stringify(shuffled);
  }

  if (input.repeat !== undefined) {
    const again = projectResolver(resolveCurrentState(corpusA, query) as Json);
    result.repeat_equal = JSON.stringify(a) === JSON.stringify(again);
  }

  if (resolveEach) {
    const states: string[] = corpusA.target_history.map((envelope: Json) =>
      (resolveCurrentState({ target_history: [envelope] }, query) as Json).state,
    );
    const unique = [...new Set(states)];
    result.all_states = unique.length === 1 ? unique[0] : states.join(",");
    result.count = states.length;
  }

  return result;
}

// ---------------------------------------------------------------------------
// capability — mirrors run_capability.
// ---------------------------------------------------------------------------

function runCapability(input: Json): Json {
  const decision = evaluateCapabilityBase(
    input.profile,
    input.actor,
    input.capability,
    input.governance_root_ref,
    input.target_scope_refs ?? [],
  ) as Json;
  if (decision.authorized) {
    return {
      authorized: decision.authorized,
      reason: null,
      grant_principal: decision.grant?.principal_ref ?? null,
    };
  }
  return {
    authorized: decision.authorized,
    reason: decision.reason ?? null,
    grant_principal: null,
  };
}

// ---------------------------------------------------------------------------
// governed write / cross-scope acceptance — mirror project_write /
// project_write_outcome (identical shapes).
// ---------------------------------------------------------------------------

function projectWriteOutcome(outcome: Json, input: Json, command: Json): Json {
  const out: Json = {};
  for (const key of [
    "disposition",
    "reason_code",
    "transition_class",
    "change_class",
    "churn_key",
    "resolved_base_recorded_at",
  ]) {
    out[key] = opt(outcome[key]);
  }
  if (outcome.record !== undefined && outcome.record !== null) {
    out.record_root = outcome.record.governance_root_ref;
  }
  if (outcome.event !== undefined && outcome.event !== null) {
    const event: Json = outcome.event;
    out.event_object_type = event.object_type;
    out.event_lifecycle = event.lifecycle_state ?? null;
    out.event_kind = opt(event.object?.event_kind);
    out.event_decision_ref = opt(event.object?.originating_decision_ref);
    out.event_origin_root = opt(event.object?.origin_governance_root_ref);
    out.event_ref = `${event.object_type}:${event.object_id}`;
  }
  if (outcome.record != null && outcome.event != null) {
    const eventRef = `${outcome.event.object_type}:${outcome.event.object_id}`;
    out.record_cites_event = (outcome.record.provenance_refs ?? []).some(
      (r: string) => r === eventRef,
    );
  }
  if (outcome.original !== undefined && outcome.original !== null) {
    const orig: Json = { ...outcome.original };
    if (orig.reason_code === undefined) orig.reason_code = null;
    out.original = orig;
  }
  out.churn_key_nonempty = outcome.churn_key != null;
  out.no_event = outcome.event == null;
  out.no_churn_key = outcome.churn_key == null;
  out.no_record = outcome.record == null;
  if (outcome.event != null) {
    out.churn_key_equals_event_ref =
      outcome.churn_key === `${outcome.event.object_type}:${outcome.event.object_id}`;
  }
  const needles = input.provenance_contains;
  if (Array.isArray(needles)) {
    const refs: string[] = outcome.record?.provenance_refs ?? [];
    out.record_provenance_contains = needles.every((n: string) => refs.includes(n));
  }
  if (input.compute_recording_lag !== undefined) {
    const recorded = epochSeconds(command.next.recorded_at);
    const valid = command.next.valid_at ? epochSeconds(command.next.valid_at) : recorded;
    out.recording_lag_seconds = recorded - valid;
  }
  return out;
}

function runWrite(input: Json): Json {
  const command: Json = input.command;
  const outcome = applyGovernedWrite(
    command,
    input.governance_root_ref,
    scopesOf(input),
  ) as Json;
  const result = projectWriteOutcome(outcome, input, command);
  if (input.run_twice !== undefined) {
    const second = applyGovernedWrite(
      command,
      input.governance_root_ref,
      scopesOf(input),
    ) as Json;
    const ref = (o: Json) => (o.event ? `${o.event.object_type}:${o.event.object_id}` : null);
    result.event_equal = ref(second) === ref(outcome);
    result.churn_key_equal = (second.churn_key ?? null) === (outcome.churn_key ?? null);
  }
  return result;
}

function runAccept(input: Json): Json {
  const command: Json = { ...input.command };
  if (input.prior_digest_of === "origin_snapshot") {
    command.prior_accepted_origin_digest = canonicalDigest(command.origin_snapshot);
  }
  const outcome = acceptCrossScope(
    command,
    input.governance_root_ref,
    scopesOf(input),
  ) as Json;
  return projectWriteOutcome(outcome, input, command);
}

// ---------------------------------------------------------------------------
// classify / stale base — direct pinned calls.
// ---------------------------------------------------------------------------

function runClassify(input: Json): Json {
  const axis: string = input.axis;
  if (!["assertion", "governance_rule", "objective_criterion"].includes(axis)) {
    throw new Error(`unknown axis ${axis}`);
  }
  return {
    change_class: classifyChangeClass(axis, input.valid_from, input.observation_started_at),
  };
}

function runStaleBase(input: Json): Json {
  const outcome = isStaleBase(input.resolved, input.base_recorded_at) as Json;
  return {
    stale: outcome.stale,
    resolved_base_recorded_at: opt(outcome.resolved_base_recorded_at),
  };
}

// ---------------------------------------------------------------------------
// promotion — mirrors run_promotion.
// ---------------------------------------------------------------------------

function runPromotion(input: Json): Json {
  // The TS command type carries evidence/endpoints/next_by_assertion as
  // ReadonlyMaps; the corpus JSON (serde-shaped) has plain objects.
  const command: Json = { ...input.command };
  for (const field of ["evidence", "endpoints", "next_by_assertion"]) {
    if (command[field] !== undefined && !(command[field] instanceof Map)) {
      command[field] = new Map(Object.entries(command[field]));
    }
  }
  const outcomes = promoteProjectionEdges(command) as Json[];
  // TS PromotionItemOutcome = { record: Promotion, kind, applied } — the
  // promotion projection fields live on .record; churn key and change class
  // ride on .applied (the applied governing transition), mirroring the Rust
  // PromotionItemOutcome flat struct + applied: Option<AppliedRecord>.
  return {
    items: outcomes.map((o) => {
      const rec = (o.record ?? {}) as Json;
      const ap = (o.applied ?? null) as Json | null;
      const delta = (ap?.delta ?? null) as Json | null;
      return {
        promotion_disposition: rec.promotion_disposition,
        reason_code: opt(rec.reason_code),
        evidence_state: rec.evidence_state,
        authority_state: rec.authority_state,
        confirmation_mode: rec.confirmation_mode,
        has_churn_key: ap !== null && ap.churn_key !== "",
        delta_change_class: delta?.change_class ?? null,
      };
    }),
  };
}

// ---------------------------------------------------------------------------
// decision surface — kernel-contract semantics (no TS module at the pin):
// exact full-ref match on applies_to_refs, input order preserved.
// ---------------------------------------------------------------------------

function runDecisionSurface(input: Json): Json {
  const items: Json[] = [];
  for (const envelope of input.decisions) {
    const object: Json = envelope.object;
    const applies: string[] = object.applies_to_refs ?? [];
    if (!applies.includes(input.resource_ref)) continue;
    items.push({
      decision_ref: `decision:${envelope.object_id}`,
      decision_kind: object.decision_kind,
      outcome: object.outcome,
      recorded_at: object.recorded_at ?? envelope.recorded_at,
    });
  }
  return { items, count: items.length };
}

// ---------------------------------------------------------------------------
// criterion — mirrors run_criterion.
// ---------------------------------------------------------------------------

function runCriterion(input: Json): Json {
  const resolved = resolveCriterion(input.objective_history, input.criterion_id) as Json;
  return {
    state: resolved.state,
    criterion_version: opt(resolved.criterion_version),
    superseded_version: opt(resolved.superseded_version),
    objective_version_recorded_at: opt(resolved.objective_version?.recorded_at),
  };
}

// ---------------------------------------------------------------------------
// churn origins — churnKeyRecordedAt (pinned model.ts) + the fork's
// touched-units rule (distinct applied_refs of in-window events).
// ---------------------------------------------------------------------------

function runChurn(input: Json): Json {
  const decisions: Json[] = input.decisions;
  const events: Json[] = input.governance_events;
  const keys: string[] = [...new Set(input.in_window_churn_keys as string[])];
  let unique = 0;
  for (const key of keys) {
    const recordedAt = churnKeyRecordedAt(key, decisions, events) as unknown as string;
    if (inInclusiveInterval(recordedAt, input.window.start, input.window.end)) unique += 1;
  }
  const touched: string[] = [];
  for (const event of events) {
    if (!inInclusiveInterval(event.recorded_at, input.window.start, input.window.end)) continue;
    const applied = event.object?.applied_ref;
    if (applied && !touched.includes(applied)) touched.push(applied);
  }
  return { unique_churn_origins: unique, touched_units: touched.length };
}

// ---------------------------------------------------------------------------
// manifest — the schema-contract compile rules (no TS compiler at the pin),
// digested with the oracle's canonicalDigest.
// ---------------------------------------------------------------------------

const DEPTHS = ["minimal", "standard", "deep"];
const OMISSION_KINDS = ["permission", "staleness", "depth_budget", "unknown"];

function runManifest(input: Json): Json {
  const compile: Json = input.compile;
  const omissions: Json[] = compile.omissions ?? [];

  const checkUnique = (refs: string[], field: string): void => {
    refs.forEach((ref, index) => {
      if (ref === "") throw new Error(`${field}[${index}] is empty`);
      if (refs.indexOf(ref) !== index)
        throw new Error(`${field}[${index}] duplicates an earlier ref: ${ref}`);
    });
  };
  const nonempty = (value: string, field: string): void => {
    if (value === "") throw new Error(`${field} is empty`);
  };

  nonempty(compile.manifest_id, "manifest_id");
  nonempty(compile.governance_root_ref, "governance_root_ref");
  nonempty(compile.activity_ref, "activity_ref");
  if (!DEPTHS.includes(compile.depth))
    throw new Error(`depth is not a frozen depth: ${compile.depth}`);
  checkUnique(compile.included_resource_refs ?? [], "included_resource_refs");
  checkUnique(compile.included_checkpoint_refs ?? [], "included_checkpoint_refs");
  checkUnique(compile.included_activity_refs ?? [], "included_activity_refs");
  checkUnique(compile.basis_checkpoint_refs ?? [], "basis_checkpoint_refs");
  checkUnique(compile.scope_refs ?? [], "scope_refs");

  omissions.forEach((o, index) => {
    if (!OMISSION_KINDS.includes(o.omission_kind))
      throw new Error(
        `omitted[${index}].omission_kind is not a frozen omission kind: ${o.omission_kind}`,
      );
    if (!o.resource_refs || o.resource_refs.length === 0)
      throw new Error(`omitted[${index}].resource_refs is empty (min 1 per contract)`);
    checkUnique(o.resource_refs, `omitted[${index}].resource_refs`);
    if (o.reason === "") throw new Error(`omitted[${index}].reason is empty`);
  });

  const permissionOmission: Json | undefined = omissions.find(
    (o) => o.omission_kind === "permission",
  );
  const permissionEnvelopeRef: string | undefined = permissionOmission
    ? `permission_envelope:${permissionOmission.resource_refs[0]}`
    : undefined;
  const completeness = permissionOmission
    ? "partial_permissions"
    : omissions.length === 0
      ? "complete"
      : "partial_context";
  if (completeness === "partial_permissions" && !permissionEnvelopeRef) {
    throw new Error(
      "partial_permissions manifest must cite the permission envelope that restricted it",
    );
  }

  const manifest: Json = {
    schema_version: "1.0.0",
    manifest_id: compile.manifest_id,
    governance_root_ref: compile.governance_root_ref,
    activity_ref: compile.activity_ref,
    depth: compile.depth,
    included_resource_refs: compile.included_resource_refs ?? [],
    completeness,
  };
  if ((compile.scope_refs ?? []).length > 0) manifest.scope_refs = compile.scope_refs;
  if ((compile.included_checkpoint_refs ?? []).length > 0)
    manifest.included_checkpoint_refs = compile.included_checkpoint_refs;
  if ((compile.included_activity_refs ?? []).length > 0)
    manifest.included_activity_refs = compile.included_activity_refs;
  if (omissions.length > 0) manifest.omitted = omissions;
  if (permissionEnvelopeRef) manifest.permission_envelope_ref = permissionEnvelopeRef;
  if ((compile.basis_checkpoint_refs ?? []).length > 0)
    manifest.basis_checkpoint_refs = compile.basis_checkpoint_refs;
  if (compile.basis_as_known_at) manifest.basis_as_known_at = compile.basis_as_known_at;
  manifest.compiled_at = compile.compiled_at;
  // Digest-input convention ratified in stage 4a (context_manifest.rs):
  // digest over the manifest with its own digest field cleared to "".
  manifest.manifest_digest = "";

  const omittedKinds: string[] = omissions.map((o) => o.omission_kind);
  return {
    completeness,
    permission_envelope_ref: opt(permissionEnvelopeRef),
    omitted_count: omittedKinds.length,
    omitted_kinds: omittedKinds,
    manifest_digest: canonicalDigest(manifest),
  };
}

// ---------------------------------------------------------------------------
// fixture cases — the pinned echo (reference) engine over the fixture
// catalog, projected onto each fixture's partition-bound step fields.
// ---------------------------------------------------------------------------

interface CatalogEntry {
  id: string;
  [key: string]: any;
}

const r1Catalog = (await loadCatalog()) as any;
const grCatalog = (await loadGrCatalog()) as any;

function catalogEntry(fixtureId: string): CatalogEntry {
  const catalog = fixtureId.startsWith("GR-") ? grCatalog : r1Catalog;
  return catalog.get(fixtureId);
}

function runFixtureCase(input: Json): Json {
  const entry = catalogEntry(input.fixture_id);
  const run = runFixture(entry, echoEngine) as Json;
  const bindings = partitionFor(entry.id) as unknown as Array<Json>;
  const items: Json[] = (run.steps as Json[]).map((_step, index) => {
    const item: Json = {};
    for (const binding of bindings) {
      if (binding.step !== index || binding.field === undefined || binding.predicate)
        continue;
      const expected = (entry.oracle?.expected ?? {})[binding.key as string];
      if (expected === undefined) continue;
      item[binding.field as string] = expected;
    }
    return item;
  });
  return { items };
}

// ---------------------------------------------------------------------------
// Dispatch + row emission.
// ---------------------------------------------------------------------------

const ADAPTERS: Record<string, (input: Json) => Json> = {
  resolver: runResolver,
  capability: runCapability,
  governed_write: runWrite,
  cross_scope_acceptance: runAccept,
  classify: runClassify,
  stale_base: runStaleBase,
  promotion: runPromotion,
  decision_surface: runDecisionSurface,
  criterion: runCriterion,
  churn_origins: runChurn,
  manifest: runManifest,
  fixture: runFixtureCase,
};

const rows: Array<{ id: string; result?: Json; error?: string }> = [];
for (const tc of corpus.cases as Json[]) {
  const id: string = tc.id as string;
  const engine: string = tc.engine as string;
  const adapter = ADAPTERS[engine];
  if (!adapter) {
    rows.push({ id, error: `no adapter for engine ${engine}` });
    continue;
  }
  try {
    rows.push({ id, result: adapter(tc.input as Json) });
  } catch (error) {
    rows.push({ id, error: error instanceof Error ? error.message : String(error) });
  }
}

fs.writeFileSync(outPath, JSON.stringify({ rows }, null, 1) + "\n");
const errors = rows.filter((r) => r.error !== undefined).length;
console.log(`ts-oracle: ${rows.length - errors} rows, ${errors} errored (presence-level parity)`);
