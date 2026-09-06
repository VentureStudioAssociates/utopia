#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# OIS-origin tooling for the stage-4b acceptance corpus (see CONVERGENCE.md).
"""Stage-4b corpus generator.

Appends the COMPLETE acceptance corpus (R1-T01..T29 + GR-E01..E10/E3B/B00)
to the fork's parity corpus as `fixture` engine cases, preserving the 94
existing engine-level cases.

Every case is derived 1:1 from the pinned oracle files at
VentureStudioAssociates/savvytinker-second-brain @ frontier/ois-v1 c56f8e5:

- fixture/oracle JSONs: fixtures/adversarial/{r1-contract-closure,gr-flagship}/
- per-step bindings: packages/testkit/src/assert/partitions{,-a,-b,-gr}.ts
  (extracted to partitions.json by extract-partitions.ts via the pinned
  testkit itself)

Case shape:
  input  = scenario facts only (common, initial, source_state,
           semantic_records, rules, operations). Expected values NEVER
           enter the input; the scenario engine computes outcomes from
           kernel engines + ported projection arithmetic.
  assert = per-step expected subsets built from the pinned partition
           bindings (key -> step/field -> oracle expected value).
           computed/predicate bindings are mirrored as explicit step
           fields, each documented in DERIVATION_NOTES below.
"""

import json
import copy
import os

ORACLE_ROOT = "/home/user/work/oracle-scratch"
DRIVER_DIR = "/home/user/work/driver"
CORPUS_PATHS = [
    "/home/user/work/utopia/crates/ois-kernel/tests/fixtures/parity-corpus.json",
    "/home/user/work/utopia/crates/ois-kernel/fixtures/parity-corpus.json",
]
PIN = "c56f8e5"

ORACLE_FILES = {
    "R1-T": (
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/oracles.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/fixtures.json",
        "r1-contract-closure",
    ),
}
GR_FILES = (
    f"{ORACLE_ROOT}/fixtures/adversarial/gr-flagship/gr-oracles.json",
    f"{ORACLE_ROOT}/fixtures/adversarial/gr-flagship/gr-fixtures.json",
    "gr-flagship",
)


def load(path):
    with open(path) as fh:
        return json.load(fh)


def load_all_oracles():
    """Returns {oracle_id: (oracle, summary)} for all 41 fixtures."""
    out = {}
    # T16-T23 summaries live in fable-r2-fixtures.json, separate from the
    # oracle files that carry their executable oracles.
    summary_files = [
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/fixtures.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/fable-r2-fixtures.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/r3-fixtures.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/gr-flagship/gr-fixtures.json",
    ]
    summaries = {}
    for fpath in summary_files:
        for f in load(fpath)["fixtures"]:
            summaries[f["id"]] = f
    for okey in [
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/oracles.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/r1-contract-closure/r3-oracles.json",
        f"{ORACLE_ROOT}/fixtures/adversarial/gr-flagship/gr-oracles.json",
    ]:
        doc = load(okey)
        for o in doc["oracles"]:
            o["_common"] = doc.get("common", {})
            o["_source_file"] = okey.replace(ORACLE_ROOT + "/", "")
            out[o["id"]] = (o, summaries[o["id"]])
    return out


def load_partition_suite():
    p = load(f"{DRIVER_DIR}/partitions.json")
    out = {}
    # R1 partitions are keyed by bare id (T01); GR by full id (GR-E01).
    for fid, bindings in p["r1"].items():
        out[f"R1-{fid}"] = bindings
    for fid, bindings in p["gr"].items():
        out[fid] = bindings
    return out


def dispositions_to_steps(bindings, expected, items):
    """computed `dispositions` -> per-step `disposition` pins."""
    disp = expected.get("dispositions")
    if disp is None:
        return
    for step, value in enumerate(disp):
        items[step]["disposition"] = value


def predicate_overrides(fid, oracle, summary, items):
    """Mirrors the pinned testkit's computed/predicate bindings as explicit
    step fields. Each entry documents the machine check it mirrors."""
    expected = oracle["expected"]
    notes = []
    if fid == "R1-T01":
        # history predicate: 'surface v1 remains nine declared units'
        items[0]["surface_versions"] = [
            {"version": 1, "declared_units": expected["surface_declared_units"]}
        ]
        notes.append("history predicate mirrored: surface v1 declared_units from oracle expected")
    elif fid == "R1-T06":
        # resolver_states predicate: every visible assertion record resolves governing
        states = {
            rec: "governing"
            for rec in oracle["semantic_records"]
            if rec.startswith("ois:assertion:")
        }
        items[0]["unit_resolver_states"] = states
        notes.append(
            "resolver_states predicate mirrored: per-record governing states over "
            "oracle semantic_records (echo-engine derivation)"
        )
    elif fid == "R1-T09":
        # history predicate: the batch retains every proposal itemwise
        supported = expected["supported_applied"]
        items[0]["batch_items"] = [
            {
                "decision_ref": f"edge-{i + 1}",
                "disposition": "applied" if i < supported else "pending_approval_or_rejected",
            }
            for i in range(summary["initial"]["proposed_edges"])
        ]
        notes.append(
            "history predicate mirrored: itemwise dispositions from oracle "
            "supported_applied split (echo-engine derivation)"
        )
    elif fid == "R1-T10":
        # history predicate: import retained without local ruling; accept never applied
        items[0]["retained_without_local_ruling"] = True
        notes.append(
            "history predicate mirrored: retained_without_local_ruling=true; "
            "accept_disposition already pinned to the oracle's "
            "rejected_or_pending_approval class"
        )
    elif fid == "GR-E09":
        # refresh_not_churn predicate over step 5 fields
        items[5]["evidence_refresh"] = expected["evidence_refresh"]
        items[5]["governing_state_change"] = expected["governing_change"]
        items[5]["touched_units"] = expected["touched_units"]
        items[5]["added_unit_count"] = expected["added_units"]
        items[5]["removed_unit_count"] = expected["removed_units"]
        notes.append(
            "refresh_not_churn predicate mirrored: expected governing_change/"
            "added_units/removed_units projected onto partition field names"
        )
    return notes


def build_case(fid, oracle, summary, bindings):
    expected = oracle["expected"]
    steps = len(oracle["operations"])
    items = [{} for _ in range(steps)]
    covered = set()
    normalization_notes = []

    for b in bindings:
        key = b["key"]
        if key not in expected:
            raise AssertionError(f"{fid}: binding key {key} missing from oracle expected")
        if b["kind"] == "step_field":
            items[b["step"]][b["field"]] = copy.deepcopy(expected[key])
            covered.add(key)
        elif b["key"] == "dispositions":
            dispositions_to_steps(bindings, expected, items)
            covered.add(key)
        elif b["kind"] in ("computed", "predicate"):
            # handled by predicate_overrides below
            covered.add(key)
        else:
            raise AssertionError(f"{fid}: unknown binding kind {b['kind']} for {key}")

    # every expected key must be covered exactly once (testkit exhaustiveness)
    missing = set(expected) - covered
    if missing:
        raise AssertionError(f"{fid}: expected keys not covered by bindings: {sorted(missing)}")

    notes = predicate_overrides(fid, oracle, summary, items)
    normalization_notes.extend(notes)

    derivation = (
        f"{oracle['_source_file']} {fid} + testkit partitions @ {PIN}; "
        f"scenario engine executes ops through kernel engines (resolver, "
        f"governed writes, promotion, criterion, churn origins) and ported "
        f"change-dynamics arithmetic (packages/projections @ {PIN})"
    )
    if normalization_notes:
        derivation += "; " + "; ".join(normalization_notes)

    case = {
        "id": f"fixture.{fid.split('-')[-1].lower().replace('r1-', 't') if fid.startswith('R1') else fid.lower().replace('gr-', 'gr_e').replace('-', '_')}",
        "engine": "fixture",
        "derivation": derivation,
        "fixture_refs": [fid],
        "input": {
            "fixture_id": fid,
            "common": oracle.get("_common", {}),
            "initial": summary.get("initial", {}),
            "source_state": oracle.get("source_state", {}),
            "semantic_records": oracle.get("semantic_records", []),
            "rules": oracle.get("rules", []),
            "operations": oracle["operations"],
        },
        "assert": {"items": items},
    }
    # Clean id: fixture.t01 .. fixture.t29, fixture.gr_e01 .. fixture.gr_b00
    if fid.startswith("R1-"):
        case["id"] = "fixture." + fid.split("-")[1].lower()
    else:
        case["id"] = "fixture." + fid.lower().replace("gr-", "gr_").replace("-", "_")
    return case


def main():
    oracles = load_all_oracles()
    partitions = load_partition_suite()

    assert set(partitions) == set(oracles), (
        f"partition/oracle mismatch: {set(partitions) ^ set(oracles)}"
    )

    corpus = load(CORPUS_PATHS[0])
    existing_ids = {c["id"] for c in corpus["cases"]}

    new_cases = []
    order = [f"R1-T{i:02d}" for i in range(1, 30)] + [
        "GR-E01", "GR-E02", "GR-E03", "GR-E04", "GR-E05", "GR-E06",
        "GR-E07", "GR-E08", "GR-E09", "GR-E10", "GR-E3B", "GR-B00",
    ]
    for fid in order:
        oracle, summary = oracles[fid]
        case = build_case(fid, oracle, summary, partitions[fid])
        assert case["id"] not in existing_ids, f"id collision: {case['id']}"
        new_cases.append(case)

    corpus["cases"].extend(new_cases)
    corpus["note"] = (
        "Differential parity corpus. Inputs derived 1:1 from the executable "
        f"oracle test suites at second-brain {PIN}; every case names its "
        "derivation and R1/GR fixture refs. Stage 4b adds the complete "
        "acceptance corpus (R1-T01..T29 + GR flagship) as fixture-engine "
        "scenario cases beside the engine-level parity cases. Both engines "
        "run identical inputs"
    )

    for path in CORPUS_PATHS:
        with open(path, "w") as fh:
            json.dump(corpus, fh, indent=1, ensure_ascii=False)
            fh.write("\n")
        print(f"wrote {path}: {len(corpus['cases'])} cases")


if __name__ == "__main__":
    main()
