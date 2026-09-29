#!/usr/bin/env python3
"""Export History assignments and reference evidence, without changing taxonomy."""
import csv
import re
import sqlite3
from decimal import Decimal
from collections import defaultdict, Counter
from pathlib import Path

DATA = Path(__file__).resolve().parents[3] / 'shared/subject-projection' / 'data'
db = sqlite3.connect(f'file:{DATA / "unified-taxonomy-v2.sqlite3"}?mode=ro', uri=True)
master = sqlite3.connect(f'file:{DATA / "master-taxonomy-v2.sqlite3"}?mode=ro', uri=True)
labels = dict(db.execute('SELECT concept_id,preferred_label FROM concept'))
parents = defaultdict(set)
for child, parent in db.execute('SELECT concept_id,parent_concept_id FROM concept_parent'):
    parents[child].add(parent)
history = {r[0] for r in db.execute('WITH RECURSIVE t(id) AS (SELECT 3319 UNION SELECT cp.concept_id FROM concept_parent cp JOIN t ON cp.parent_concept_id=t.id) SELECT id FROM t')}
reference = defaultdict(set)
for system, code, label in master.execute('SELECT system_id,selector,preferred_label FROM source_selector JOIN concept USING(concept_id)'):
    reference[system, code.split('@')[0]].add(label)
assignments = []
for cid, lo, hi in db.execute('SELECT concept_id,start_code,end_code FROM lcc_range'):
    assignments.append(('lcc_range',cid,f'{lo}..{hi}'))
for system in ('lcc','bisac','ddc'):
    assignments.extend((system,cid,code) for cid,code in db.execute(f'SELECT concept_id,selector FROM {system}_selector'))
owners = defaultdict(set)
for system,cid,code in assignments:
    owners[system,code].add(cid)

stop = set('history historical of the and in to by general period era modern early late century centuries &'.split())
def words(s):
    return set(re.findall(r'[a-z]{3,}',s.lower()))-stop

rows = []
for system,cid,code in assignments:
    if cid not in history:
        continue
    heads = reference[system.replace('_range',''), code.split('@')[0]]
    rows.append((system, cid, labels[cid], ' | '.join(labels[p] for p in sorted(parents[cid])), code,
                 ' | '.join(sorted(heads)), ' | '.join(f'{i}: {labels[i]}' for i in sorted(owners[system,code]-{cid})),
                 bool(heads) and not any(words(labels[cid]) & words(h) for h in heads)))
output=DATA/'reports/20260915_history_assignment_review.csv'
with output.open('w',newline='') as stream:
    writer=csv.writer(stream)
    writer.writerow(('system','concept_id','label','parents','code','exact_master_headings','other_owners','lexical_review_candidate'))
    writer.writerows(rows)
print('subjects',len(history),'assignments',len(rows),Counter(r[0] for r in rows))
print('Exact master evidence',sum(bool(r[5]) for r in rows))
print('Multiply assigned codes',len({(r[0],r[4]) for r in rows if r[6]}))
print('Lexical candidates',sum(r[7] for r in rows))
# Country membership is a stronger screening signal than label word overlap.
country_ids = set(map(int, '''3321 3322 3324 3326 3327 3328 3333 3334 3337 3338
3339 3340 3341 3342 3343 3345 3346 3347 3348 3350 3351 3352 3353 3354 3355
3356 3360 3361 3362 3363 3364 3365 3367 3370 3371 3372 3375 3380 3381 3382
3383 3387 3388 3389 3390 3391 3392 3393 3394 3400 3402 3426 3427 3428 3429
3430 3486 3488 3491 3492 3493 3496 3497 3498 3500 3503 3504 3505 3506 3507
3510 3511 3513 3523 3524 3530 3534 3535 3537 3538 3539 3540 3544 3545 3546
3548 3549 3551 3554 3566 3574 3577 3578 3579 3580 3581 3582 3583 3584 3586
3587 3589 3590 3591 3594 3595 3596 3597 3598 3602 3603 3606 3607 3610 3611
3613 3635 3636 3637 3639 3677 3681'''.split()))
master_parents=defaultdict(set)
for child,parent in master.execute('SELECT concept_id,parent_concept_id FROM concept_parent'):
    master_parents[child].add(parent)
def ancestors(cid, graph):
    seen, pending=set(),[cid]
    while pending:
        node=pending.pop()
        if node in seen: continue
        seen.add(node)
        pending.extend(graph[node])
    return seen
ref_countries=defaultdict(set)
for system,code,cid in master.execute('SELECT system_id,selector,concept_id FROM source_selector'):
    ref_countries[system,code.split('@')[0]].update(ancestors(cid,master_parents)&country_ids)
geography=[]
for row in rows:
    system,cid,label,_,code,headings=row[:6]
    actual=ancestors(cid,parents)&country_ids
    expected=ref_countries[system.replace('_range',''),code.split('@')[0]]
    if actual and expected and actual.isdisjoint(expected):
        geography.append((cid,label,code,headings,' | '.join(labels[i] for i in sorted(actual)),
                          ' | '.join(labels[i] for i in sorted(expected))))
with (DATA/'reports/20260915_history_geography_candidates.csv').open('w',newline='') as stream:
    writer=csv.writer(stream)
    writer.writerow(('concept_id','label','code','reference_headings','curated_countries','reference_countries'))
    writer.writerows(geography)
print('Country-context discrepancies',len(geography))
for row in geography: print(' | '.join(map(str,row)))
for (system,code),ids in sorted(owners.items()):
    if system!='bisac' or not (ids & history) or len(ids)<2: continue
    shadowed=set().union(*(ancestors(cid,parents)-{cid} for cid in ids))
    remaining=ids-shadowed
    print('BISAC conflict',code,'remaining unrelated candidates',len(remaining))

def numeric(code):
    match=re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)',code)
    return (match[1],Decimal(match[2])) if match else None
country_spans={}
for cid,code in master.execute("SELECT concept_id,selector FROM source_selector WHERE system_id='lcc'"):
    bounds=code.split('@')[0].split('..')
    if len(bounds)!=2: continue
    lo,hi=bounds
    a,b=numeric(lo),numeric(hi)
    if cid in country_ids and a and b and a[0]==b[0]:
        previous=country_spans.get(cid)
        if previous is None or b[1]-a[1]>previous[2]-previous[1]:
            country_spans[cid]=(a[0],a[1],b[1])
outliers=[]
for row in rows:
    system,cid,label,_,code,headings=row[:6]
    if system!='lcc_range': continue
    bounds=code.split('..')
    a,b=map(numeric,bounds)
    if not a or not b: continue
    countries=ancestors(cid,parents)&country_spans.keys()
    if not countries: continue
    if all(a[0]!=country_spans[n][0] or a[1]<country_spans[n][1] or b[1]>country_spans[n][2] for n in countries):
        outliers.append((cid,label,code,headings,' | '.join(f'{labels[n]}: {country_spans[n]}' for n in sorted(countries))))
with (DATA/'reports/20260915_history_range_outliers.csv').open('w',newline='') as stream:
    writer=csv.writer(stream)
    writer.writerow(('concept_id','label','code','reference_headings','country_span_screen'))
    writer.writerows(outliers)
print('Outside widest country numeric range (screen only)',len(outliers))
for row in outliers:
    if row[0] in country_ids: print(' | '.join(map(str,row)))
