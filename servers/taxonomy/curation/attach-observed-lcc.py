#!/usr/bin/env python3
"""Propose observed non-law LCC selectors using evidenced reference ancestors.

Reads a full lcc_coverage CSV and an immutable taxonomy snapshot. It does not
modify the published taxonomy. Proposed mappings and unresolved entries retain
their schedule paths for review; use lcc_coverage again on the staged database
before publishing any migration.
"""

import argparse
import csv
import json
import re
import sqlite3
import sys
from collections import Counter, defaultdict
from functools import lru_cache
from pathlib import Path


def csv_rows(path):
    with Path(path).open() as source:
        yield from csv.DictReader(source)


def normalized(value):
    return " ".join(re.findall(r"\w+", value.casefold()))


# These captions are relative to their parent, not reliable topic names.
GENERIC = {"history", "general", "general works", "special", "special topics",
           "other", "miscellaneous", "by period", "by country", "english",
           "french", "german", "spanish", "individual authors", "authors"}


def readonly(path):
    return sqlite3.connect(f"file:{Path(path).resolve()}?mode=ro", uri=True)


def generate(taxonomy, reference, coverage, output, master=None, reviewed=None, candidates=None):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    db = readonly(taxonomy)
    labels = dict(db.execute("SELECT concept_id,preferred_label FROM concept"))
    parents = defaultdict(set)
    for child, parent in db.execute("SELECT concept_id,parent_concept_id FROM concept_parent"):
        parents[child].add(parent)

    @lru_cache(None)
    def ancestors(concept):
        seen = {concept}
        pending = list(parents[concept])
        while pending:
            item = pending.pop()
            if item not in seen:
                seen.add(item)
                pending.extend(parents[item])
        return frozenset(seen)

    @lru_cache(None)
    def ancestor_labels(concept):
        result = defaultdict(set)
        for item in ancestors(concept):
            result[normalized(labels[item])].add(item)
        return result

    known = {}
    for row in csv_rows(coverage):
        if row["kind"] != "selector_conflict":
            known[row["code"]] = (row["kind"], row["concept_ids"])

    # Range anchors require an identical stored selector, never a guessed
    # classification based on the number at one end of a range.
    spans = defaultdict(set)
    for concept, selector in db.execute("SELECT concept_id,selector FROM source_selector WHERE system_id='lcc'"):
        if ".." in selector:
            start, end = selector.split("@", 1)[0].split("..", 1)
            spans[(start, end)].add(concept)
    if master:
        old = readonly(master)
        for concept, label, selector in old.execute("""
            SELECT c.concept_id,c.preferred_label,s.selector FROM concept c
            JOIN source_selector s USING(concept_id)
            WHERE s.system_id='lcc' AND s.selector LIKE '%..%'
        """):
            # Historical ranges are evidence for unchanged, retained concepts,
            # not new permissive selectors to restore into the curated tree.
            if labels.get(concept) != label:
                continue
            start, end = selector.split("@", 1)[0].split("..", 1)
            if start != end:
                spans[(start, end)].add(concept)

    if master:
        old.close()
    ref = readonly(reference)
    records = {}
    for rid, code, end, caption in ref.execute("""
        SELECT r.record_id,s.start_number,coalesce(nullif(s.end_number,''),s.start_number),r.caption
        FROM reference_current_record r JOIN classification_span s USING(record_id)
        WHERE coalesce(s.table_id,'')='' AND s.ordinal=0
    """):
        if re.fullmatch(r"[A-Z]{1,4}\d[\d.A-Za-z ]*", code):
            records[rid] = (code, end, caption or "")
    hierarchy = defaultdict(list)
    for rid, label in ref.execute("SELECT record_id,label FROM caption_hierarchy ORDER BY record_id,ordinal"):
        if rid in records:
            hierarchy[rid].append(sys.intern(label))

    # An anchor is supported either by a currently classified record, an
    # identical range selector, or matching ancestor labels on BOTH trees.
    # Scope paths to their LCC subclass to avoid same-name cross-schedule trees.
    anchors = defaultdict(set)
    paths = {}
    evidence = {}
    for rid, (code, end, caption) in records.items():
        scope = re.match(r"[A-Z]+", code)[0]
        raw = tuple(hierarchy[rid]) + (caption,)
        path = tuple(sys.intern(normalized(part)) for part in raw)
        paths[rid] = (scope, path, raw)
        targets = spans.get((code, end), set()) if end != code else set()
        if end == code and known.get(code, (None,))[0] == "matched":
            targets = {int(known[code][1])}
        for target in targets:
            anchors[(scope, path)].add(target)
            evidence.setdefault((scope, path, target), f"existing selector: {code}..{end}")
            aligned = ancestor_labels(target)
            for length, component in enumerate(path, 1):
                if component in GENERIC:
                    continue
                for ancestor in aligned.get(component, ()):
                    key = (scope, path[:length])
                    anchors[key].add(ancestor)
                    evidence.setdefault((*key, ancestor), f"aligned ancestor of {code}: {labels[ancestor]}")


    # Prefer a named descendant over its own ancestor when both evidence
    # sources describe exactly the same schedule node. Unrelated targets
    # remain unresolved; never choose one by insertion order.
    for key, targets in anchors.items():
        if len(targets) > 1:
            redundant = set().union(*(ancestors(t) - {t} for t in targets))
            targets.difference_update(redundant)

    reviews = json.loads(Path(reviewed).read_text()) if reviewed else {}
    for item in reviews.get("anchors", []):
        target = item["concept_id"]
        if labels.get(target) != item["subject"]:
            raise ValueError(f"Reviewed subject changed: {item}")
        path = tuple(normalized(part) for part in item["path"])
        key = (item["scope"], path)
        anchors[key] = {target}
        evidence[(*key, target)] = "reviewed schedule-to-subject equivalence"

    # Resolve observed notations through exact schedule records first. Bare
    # section numbers may use a verified enclosing numeric schedule span.
    # Printed ranges require the exact two endpoints; never infer from one end.
    candidate_records = []
    exact = defaultdict(list)
    numeric_spans = defaultdict(list)
    def numeric(code):
        m = re.fullmatch(r"([A-Z]+)(\d+(?:\.\d+)?)", code)
        return (m[1], float(m[2])) if m else None
    for rid, (start, end, caption) in records.items():
        exact[(start, end)].append(rid)
        a, b = numeric(start), numeric(end)
        if a and b and a[0] == b[0] and a[1] <= b[1]:
            numeric_spans[a[0]].append((a[1], b[1], rid))
    unresolved_input = []
    for row in csv_rows(candidates):
        code = row['lcc_string']
        # Candidate CSVs can predate the coverage audit. Do not turn a matched
        # or ambiguous code back into an unmatched proposal.
        if code in known and known[code][0] != 'unmatched':
            continue
        assert not code.startswith('K'), 'Law excluded'
        ids = []
        if '-' in code:
            m = re.fullmatch(r"([A-Z]+)(\d+(?:\.\d+)?)-(?:([A-Z]+))?(\d+(?:\.\d+)?)", code)
            if m and (not m[3] or m[1] == m[3]):
                ids = exact.get((m[1]+m[2], m[1]+m[4]), [])
        elif re.fullmatch(r"[A-Z]+\d[\d.A-Z ]*", code):
            ids = exact.get((code, code), [])
            if not ids:
                m = re.match(r"([A-Z]+)(\d+(?:\.\d+)?)", code)
                spans_here = [(b-a, rid) for a,b,rid in numeric_spans[m[1]] if a <= float(m[2]) <= b]
                if spans_here:
                    width = min(x[0] for x in spans_here)
                    ids = [rid for w,rid in spans_here if w == width]
        if not ids:
            unresolved_input.append((code, '', 'no verified exact range or containing schedule record', ''))
        for rid in ids:
            candidate_records.append((rid, (code, code, records[rid][2])))
            known[code] = ('unmatched', '')

    proposals = defaultdict(list)
    rejected = defaultdict(list)
    for rid, (code, end, caption) in candidate_records:
        if code != end or known.get(code, (None,))[0] != "unmatched":
            continue
        scope, path, raw = paths[rid]
        found = False
        for length in range(len(path), 0, -1):
            targets = anchors.get((scope, path[:length]))
            if not targets:
                continue
            found = True
            if len(targets) != 1:
                rejected[code].append((rid, "conflicting ancestor destinations", " / ".join(raw)))
                break
            target = next(iter(targets))
            proposals[code].append((target, rid, " / ".join(raw), " / ".join(raw[:length]), evidence[(scope, path[:length], target)]))
            break
        if not found:
            rejected[code].append((rid, "no evidenced ancestor", " / ".join(raw)))

    accepted = []
    for code, choices in sorted(proposals.items()):
        targets = {item[0] for item in choices}
        if len(targets) != 1 or code in rejected:
            for target, rid, path, anchor, why in choices:
                rejected[code].append((rid, "inconsistent reference records", path))
            continue
        target, rid, path, anchor, why = choices[0]
        accepted.append((code, target, labels[target], rid, path, anchor, why))

    explicit = {item["code"]: item for item in reviews.get("codes", [])}
    accepted = [row for row in accepted if row[0] not in explicit]
    for code, item in explicit.items():
        target = item["concept_id"]
        if labels.get(target) != item["subject"]:
            raise ValueError(f"Reviewed subject changed: {item}")
        if code in known and known[code][0] != "unmatched":
            continue
        accepted.append((code, target, labels[target], "", item["meaning"], item["meaning"], "individually reviewed code/section stem"))
        rejected.pop(code, None)
    accepted.sort()

    with (output / "proposals.csv").open("w") as f:
        writer = csv.writer(f)
        writer.writerow(["lcc_code", "concept_id", "subject", "reference_record_id", "schedule_path", "anchor_path", "evidence"])
        writer.writerows(accepted)
    with (output / "unresolved.csv").open("w") as f:
        writer = csv.writer(f)
        writer.writerow(["lcc_code", "reference_record_id", "reason", "schedule_path"])
        writer.writerows((code, *item) for code, items in sorted(rejected.items()) for item in items)
        writer.writerows(unresolved_input)
    stats = {"proposed_codes": len(accepted), "receiving_subjects": len({r[1] for r in accepted}), "unresolved_codes": len(set(rejected) | {r[0] for r in unresolved_input}), "largest_destinations": Counter(r[2] for r in accepted).most_common(25)}
    (output / "summary.json").write_text(json.dumps(stats, indent=2) + "\n")
    print(json.dumps(stats, indent=2))
    ref.close()
    db.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("taxonomy")
    parser.add_argument("reference")
    parser.add_argument("coverage")
    parser.add_argument("output")
    parser.add_argument("--candidates", required=True, help="Observed non-law unmatched notation CSV")
    parser.add_argument("--master", help="Historical range evidence for retained, unchanged concepts")
    parser.add_argument("--reviewed", help="Explicitly reviewed schedule aliases and individual codes")
    args = parser.parse_args()
    generate(args.taxonomy, args.reference, args.coverage, args.output, args.master, args.reviewed, args.candidates)
