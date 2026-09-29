#!/usr/bin/env python3
"""Maintain curated selector syntax; export master additions as proposals only.

The curated database owns its assignments and range boundaries. Maintenance
never restores master selectors, infers wildcards, or merges overlapping ranges:
all three can undo curation or change the winning subject. Master additions must
be reviewed and checked with the production matcher in a separate migration.
"""

import argparse
import csv
import re
import sqlite3
from collections import defaultdict, deque
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3] / 'shared/subject-projection'
MASTER = ROOT / "data" / "master-taxonomy-v2.sqlite3"
CURATED = ROOT / "data" / "unified-taxonomy-v2.sqlite3"
CURATED_SYSTEMS = {"bisac", "lcc"}


def canonical_lcc_selector(selector):
    if ".." not in selector:
        return selector
    bounds = re.sub(r"@[0-9]+$", "", selector)
    start, end = (part.strip() for part in bounds.split("..", 1))
    if end and end[0].isdigit():
        letters = re.match(r"[A-Za-z]+", start)
        if letters:
            end = letters.group() + end
    return f"{start}..{end}"


def uses_split_selectors(connection):
    return connection.execute(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='lcc_selector'"
    ).fetchone()[0]


def selector_rows(connection):
    if uses_split_selectors(connection):
        return set(connection.execute(
            "SELECT concept_id,'lcc',selector FROM lcc_selector "
            "UNION ALL SELECT concept_id,'bisac',selector FROM bisac_selector"
        ))
    return set(connection.execute("SELECT concept_id,system_id,selector FROM source_selector"))


def compile_selectors(curated):
    """Canonicalize existing rows without changing destinations or coverage."""
    planned = defaultdict(set)
    for concept, system, selector in selector_rows(curated):
        planned[(concept, system)].add(canonical_lcc_selector(selector) if system == "lcc" else selector)
    return planned


def nearest_retained(concept_id, parents, retained, cache):
    if concept_id in cache:
        return cache[concept_id]
    queue = deque([(concept_id, 0)])
    seen, found, found_depth = set(), set(), None
    while queue:
        item, depth = queue.popleft()
        if item in seen or (found_depth is not None and depth > found_depth):
            continue
        seen.add(item)
        if item in retained:
            found.add(item)
            found_depth = depth
        else:
            queue.extend((parent, depth + 1) for parent in parents.get(item, ()))
    cache[concept_id] = found
    return found


def numeric_bounds(selector):
    """Conservative coverage check, never interpret printed hyphen labels."""
    if ".." in selector:
        match = re.fullmatch(r"([A-Z]+)(\d+(?:\.\d+)?)\.\.([A-Z]+)(\d+(?:\.\d+)?)", selector)
        if not match or match[1] != match[3]:
            return None
        start, end = Decimal(match[2]), Decimal(match[4])
        return (match[1], start, end) if start <= end else None
    if "-" in selector or "*" in selector:
        return None
    match = re.match(r"^([A-Z]+)\s*(\d+(?:\.\d+)?)", selector)
    return (match[1], Decimal(match[2]), Decimal(match[2])) if match else None


def propose_master_additions(master, curated):
    planned = compile_selectors(curated)
    labels = dict(curated.execute("SELECT concept_id,preferred_label FROM concept"))
    master_labels = dict(master.execute("SELECT concept_id,preferred_label FROM concept"))
    normalize = lambda label: " ".join(re.findall(r"\w+", label.casefold()))
    # IDs created independently in curated and master can collide. An ID alone
    # is not evidence that a concept has retained its original meaning.
    retained = {cid for cid, label in labels.items() if cid in master_labels and normalize(label) == normalize(master_labels[cid])}
    parents = defaultdict(set)
    for child, parent in master.execute("SELECT concept_id,parent_concept_id FROM concept_parent"):
        parents[child].add(parent)
    assigned = {(system, selector) for (_, system), selectors in planned.items() for selector in selectors}
    ranges = defaultdict(list)
    for (cid, system), selectors in planned.items():
        if system == "lcc":
            for selector in selectors:
                bounds = numeric_bounds(selector) if ".." in selector else None
                if bounds:
                    ranges[(cid, bounds[0])].append(bounds[1:])
    candidates, cache, unresolved = set(), {}, 0
    for cid, system, raw in sorted(selector_rows(master)):
        if system not in CURATED_SYSTEMS:
            continue
        selector = canonical_lcc_selector(raw) if system == "lcc" else raw
        # Explicitly moved selectors must not be restored to an old destination.
        if (system, selector) in assigned:
            continue
        targets = nearest_retained(cid, parents, retained, cache)
        if not targets:
            unresolved += 1
        bounds = numeric_bounds(selector) if system == "lcc" else None
        for target in targets:
            if bounds and any(lo <= bounds[1] and bounds[2] <= hi for lo, hi in ranges[(target, bounds[0])]):
                continue
            candidates.add((target, labels[target], system, selector, cid, master_labels[cid]))
    return sorted(candidates), unresolved


def apply(curated, planned, expected_rows):
    curated.execute("BEGIN IMMEDIATE")
    try:
        if selector_rows(curated) != expected_rows:
            raise RuntimeError("Curated selectors changed during planning; rerun maintenance")
        wanted = {(cid, system, selector) for (cid, system), selectors in planned.items() for selector in selectors}
        # Apply only canonicalization differences, preserving unrelated rows.
        # Write to whichever selector schema this database has; the reader
        # accepts both, so the writer must too.
        split = uses_split_selectors(curated)
        for concept, system, selector in sorted(expected_rows - wanted):
            if split:
                curated.execute(f"DELETE FROM {system}_selector WHERE concept_id=? AND selector=?", (concept, selector))
            else:
                curated.execute("DELETE FROM source_selector WHERE concept_id=? AND system_id=? AND selector=?", (concept, system, selector))
        for concept, system, selector in sorted(wanted - expected_rows):
            if split:
                curated.execute(f"INSERT INTO {system}_selector(concept_id,selector) VALUES(?,?)", (concept, selector))
            else:
                curated.execute("INSERT INTO source_selector(concept_id,system_id,selector) VALUES(?,?,?)", (concept, system, selector))
        curated.commit()
    except Exception:
        curated.rollback()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--curated", type=Path, default=CURATED)
    parser.add_argument("--master", type=Path, default=MASTER)
    parser.add_argument("--apply", action="store_true", help="Apply syntax canonicalization only; never import master selectors")
    parser.add_argument("--master-proposals", type=Path, help="Export candidate additions as CSV without modifying either taxonomy")
    args = parser.parse_args()
    if args.apply and args.master_proposals:
        parser.error("Master additions are proposals only; --master-proposals cannot be combined with --apply")
    mode = "rw" if args.apply else "ro"
    with sqlite3.connect(f"file:{args.curated.resolve()}?mode={mode}", uri=True) as curated:
        before = selector_rows(curated)
        planned = compile_selectors(curated)
        after = sum(map(len, planned.values()))
        print(f"maintenance selectors: {len(before)} -> {after} ({after-len(before):+d}); master imports: 0")
        if args.master_proposals:
            with sqlite3.connect(f"file:{args.master.resolve()}?mode=ro", uri=True) as master:
                candidates, unresolved = propose_master_additions(master, curated)
            with args.master_proposals.open("w", newline="") as out:
                writer = csv.writer(out)
                writer.writerow(["concept_id", "label", "system_id", "selector", "master_concept_id", "master_label"])
                writer.writerows(candidates)
            print(f"master proposals: {len(candidates)}; unresolved master selectors: {unresolved}; output: {args.master_proposals}")
        if args.apply:
            apply(curated, planned, before)
            print("applied syntax maintenance only")


if __name__ == "__main__":
    main()
