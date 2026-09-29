#!/usr/bin/env python3
"""Read-only audit of dated History labels against bundled master code headings."""
import csv
import re
import sqlite3
from collections import defaultdict, Counter
from pathlib import Path

DATA = Path(__file__).resolve().parents[3] / 'shared/subject-projection' / 'data'
db = sqlite3.connect(f'file:{DATA / "unified-taxonomy-v2.sqlite3"}?mode=ro', uri=True)
master = sqlite3.connect(f'file:{DATA / "master-taxonomy-v2.sqlite3"}?mode=ro', uri=True)
labels = dict(db.execute('SELECT concept_id,preferred_label FROM concept'))
children = defaultdict(set)
for child, parent in db.execute('SELECT concept_id,parent_concept_id FROM concept_parent'):
    children[parent].add(child)

def descendants(cid):
    seen, pending = set(), list(children[cid])
    while pending:
        node = pending.pop()
        if node in seen:
            continue
        seen.add(node)
        pending.extend(children[node])
    return seen

codes = defaultdict(list)
for cid, lo, hi in db.execute('SELECT concept_id,start_code,end_code FROM lcc_range'):
    codes[cid].append(('lcc', f'{lo}..{hi}'))
for system in ('lcc', 'bisac', 'ddc'):
    for cid, code in db.execute(f'SELECT concept_id,selector FROM {system}_selector'):
        codes[cid].append((system, code))

exact, starts = defaultdict(set), defaultdict(set)
for system, code, label in master.execute('SELECT s.system_id,s.selector,c.preferred_label FROM source_selector s JOIN concept c USING(concept_id)'):
    code = code.split('@')[0]
    exact[system, code].add(label)
    starts[system, code.split('..')[0]].add(label)

def years(label):
    return set(re.findall(r'(?<!\d)\d{1,4}(?!\d)', label))

def normalized_label(label):
    return re.sub(r'[^\w]', '', label.casefold())

rows = []
for cid in sorted(descendants(3319)):
    label = labels[cid]
    if not re.search(r'\d', label):
        continue
    evidence, anchors = set(), set()
    for system, code in codes[cid]:
        normalized = code.split('@')[0]
        evidence.update(exact[system, normalized])
        anchors.update(starts[system, normalized.split('..')[0]])
    dated = {s for s in evidence if years(s)}
    if not codes[cid]:
        coded_descendants = sum(bool(codes[n]) for n in descendants(cid))
        status = 'uncoded grouping' if coded_descendants else 'uncoded leaf/subtree'
    elif any(normalized_label(label) == normalized_label(s) for s in evidence):
        status = 'same label in exact-code heading (review scope)'
    elif years(label) and any(years(label) == years(s) for s in dated):
        status = 'same years in exact-code heading (review scope)'
    elif dated:
        status = 'different years in exact-code headings (review)'
    else:
        status = 'no exact dated heading (review)'
    rows.append((cid,label,status,'; '.join(f'{s}:{c}' for s,c in codes[cid]),
                 ' | '.join(sorted(evidence)), ' | '.join(sorted(anchors-evidence))))

output = DATA / 'reports/20260915_history_period_code_audit.csv'
with output.open('w', newline='') as stream:
    writer = csv.writer(stream)
    writer.writerow(('concept_id','label','status','codes','exact_code_headings','same_start_headings'))
    writer.writerows(rows)
print(output)
print(Counter(row[2] for row in rows))
