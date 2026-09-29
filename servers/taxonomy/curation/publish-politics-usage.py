#!/usr/bin/env python3
"""Publish full-corpus direct counts against the exact applied audit snapshot.

Unlike the older exporter, path enumeration does not conflate distinct IDs when
unrelated existing subjects share display paths. The matcher audit still uses
the production engine with unchanged IDs, selectors and parent relationships.
"""
import csv
from functools import lru_cache
import importlib.util
from pathlib import Path
import sqlite3
import sys

folder=Path(sys.argv[1])
spec=importlib.util.spec_from_file_location('politics',Path(__file__).with_name('improve-politics.py'))
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with sqlite3.connect(f'file:{module.DATABASE}?mode=ro',uri=True) as live, sqlite3.connect(folder/'after.sqlite3') as frozen:
    assert module.snapshot(live)==module.snapshot(frozen),'Live taxonomy differs from audited snapshot'
    labels=dict(live.execute('SELECT concept_id,preferred_label FROM concept'))
    parents={}
    for child,parent in live.execute('SELECT concept_id,parent_concept_id FROM concept_parent'):
        parents.setdefault(child,[]).append(parent)
    counts={int(id):int(count) for id,count in csv.reader((folder/'full-counts.tsv').open(),delimiter='\t')}
    @lru_cache(None)
    def paths(id):
        if id not in parents:return (labels[id],)
        return tuple(sorted({path+' / '+labels[id] for parent in parents[id] for path in paths(parent)}))
    output=module.DATABASE.parent/'taxonomy-usage-openlibrary-2026-07-31.tsv'
    staging=output.with_suffix('.politics-building.tsv')
    with staging.open('w',newline='') as dest:
        writer=csv.writer(dest,delimiter='\t',lineterminator='\n')
        writer.writerow(['concept_id','direct_hits','path'])
        rows=[(id,count,path) for id,count in counts.items() for path in paths(id) if count]
        for row in sorted(rows,key=lambda r:(-r[1],r[2],r[0])):writer.writerow(row)
    staging.replace(output)
    print(f'Published {len(rows)} usage paths from full corpus audit')
