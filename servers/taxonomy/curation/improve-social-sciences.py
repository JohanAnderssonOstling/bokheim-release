#!/usr/bin/env python3
"""Reviewed Social Sciences cleanup. Preview first; apply only an unchanged plan.

The before database and row-level manifest make every change recoverable.
This is a one-time migration, not a generic taxonomy normalizer.
"""
import argparse
from decimal import Decimal
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import tempfile

spec = importlib.util.spec_from_file_location('baseline', Path(__file__).with_name('improve-politics.py'))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
ROOT, DATABASE = baseline.ROOT, baseline.DATABASE
snapshot, validate = baseline.snapshot, baseline.validate


def improve(db):
    manifest = {'merges': {}, 'created': {}, 'removed_contained_ranges': [], 'added_matching_ranges': []}

    def label(id, text):
        assert db.execute('UPDATE concept SET preferred_label=? WHERE concept_id=?', (text, id)).rowcount == 1

    def link(id, parent):
        if db.execute('SELECT 1 FROM concept_parent WHERE concept_id=? AND parent_concept_id=?', (id, parent)).fetchone():
            return
        ordinal = db.execute('SELECT coalesce(max(ordinal),-1)+1 FROM concept_parent WHERE concept_id=?', (id,)).fetchone()[0]
        db.execute('INSERT INTO concept_parent VALUES(?,?,?)', (id, parent, ordinal))

    def move(id, parent, old=None):
        if old is None:
            db.execute('DELETE FROM concept_parent WHERE concept_id=?', (id,))
        else:
            db.execute('DELETE FROM concept_parent WHERE concept_id=? AND parent_concept_id=?', (id, old))
        link(id, parent)

    def create(text, parent):
        id = db.execute('INSERT INTO concept(preferred_label) VALUES(?)', (text,)).lastrowid
        manifest['created'][text] = id
        link(id, parent)
        return id

    def merge(old, new, keep_parents=False):
        assert old != new
        assert db.execute('SELECT 1 FROM concept WHERE concept_id=?', (old,)).fetchone()
        if keep_parents:
            for (parent,) in db.execute('SELECT parent_concept_id FROM concept_parent WHERE concept_id=?', (old,)).fetchall():
                if parent != new:
                    link(new, parent)
        for (child,) in db.execute('SELECT concept_id FROM concept_parent WHERE parent_concept_id=?', (old,)).fetchall():
            if child != new:
                link(child, new)
        for table in ('lcc_selector', 'bisac_selector'):
            db.execute(f'INSERT OR IGNORE INTO {table} SELECT ?,selector FROM {table} WHERE concept_id=?', (new, old))
            db.execute(f'DELETE FROM {table} WHERE concept_id=?', (old,))
        for table in ('lcc_range', 'ddc_selector'):
            db.execute(f'UPDATE {table} SET concept_id=? WHERE concept_id=?', (new, old))
        for (text,) in db.execute('SELECT label FROM source_label WHERE concept_id=? ORDER BY ordinal', (old,)).fetchall():
            ordinal = db.execute('SELECT coalesce(max(ordinal),-1)+1 FROM source_label WHERE concept_id=?', (new,)).fetchone()[0]
            db.execute('INSERT OR IGNORE INTO source_label VALUES(?,?,?)', (new, text, ordinal))
        db.execute('DELETE FROM source_label WHERE concept_id=?', (old,))
        db.execute('DELETE FROM concept_parent WHERE concept_id=? OR parent_concept_id=?', (old, old))
        db.execute('DELETE FROM concept WHERE concept_id=?', (old,))
        manifest['merges'][old] = new

    def transfer_range(lo, hi, old, new):
        assert db.execute('UPDATE lcc_range SET concept_id=? WHERE start_code=? AND end_code=? AND concept_id=?',
                          (new, lo, hi, old)).rowcount == 1

    # DDC: broad codes must not imply narrower subject matter. 655 is unassigned
    # in current DDC; no unsupported legacy destination is invented.
    for code, old, new in [('381',12269,5083), ('304',12260,5488), ('152',5389,5381)]:
        assert db.execute('UPDATE ddc_selector SET concept_id=? WHERE selector=? AND concept_id=?', (new,code,old)).rowcount == 1
    assert db.execute("DELETE FROM ddc_selector WHERE selector='655' AND concept_id=5236").rowcount == 1
    label(53287, 'Unconscious Mind & Altered States')

    # HD includes management, land use and labor, not just industries.
    transfer_range('HD28','HD9999',5157,5104)
    for (selector,) in db.execute('SELECT selector FROM lcc_selector WHERE concept_id=5157').fetchall():
        db.execute('INSERT OR IGNORE INTO lcc_selector VALUES(?,?)', (5104,selector))
    db.execute('DELETE FROM lcc_selector WHERE concept_id=5157')
    for id in (5232,5234,5235,12272):
        move(id,5104,5157)
    # Navigation is already an appropriate transportation destination. A broader
    # label also accommodates the commercial marine portion of VK.
    label(478,'Navigation & Merchant Marine')
    transfer_range('VK1','VK1661',13770,478)

    # Actual Persons and Family Law scopes, rather than merging misleading labels.
    label(11731,'Private Law Unification')
    merge(11732,5310,True)
    transfer_range('K670','K709',5281,5288)
    link(5288,5310)
    reference = create('Reference & Collections',5276)
    for lo,hi in [('K37','K44'),('K50','K54'),('K68','K70'),('K181','K184.7'),('K183','K184.7')]:
        transfer_range(lo,hi,5555,reference)
    assert db.execute("UPDATE lcc_selector SET concept_id=? WHERE concept_id=5555 AND selector='K48'", (reference,)).rowcount == 1
    move(51562,reference,5555)

    # Merge classification-system duplicates, preserving meaningful cross-links.
    for old,new in [(5061,5058),(5070,5069),(5096,5239),(5382,5398),
                    (10785,5407),(10835,5417),(5404,5418),(11004,5419),(10794,5421)]:
        merge(old,new)
    merge(5251,5496,True)
    merge(53282,5493,True)
    merge(5502,12294,True)
    link(5239,5664)
    label(5057,'Fields')
    label(5058,'Cultural & Social')
    label(5069,'Physical')
    label(5067,'Human Evolution')
    label(5071,'Prehistoric')
    move(5071,5492,5057)
    link(5492,5056)
    # Whole/parts psychology is related to Gestalt, not a second identical node.
    label(10990,'Whole & Parts')
    move(10990,5418)

    # These are sociological classes/communities, not exclusively anthropology.
    label(5072,'Communities, Classes & Race')
    move(5072,5488)
    label(5075,'Race & Race Relations')
    link(5073,5663)
    link(5075,5663)

    # A broad HQ fallback, with distinct family, gender and life-stage children.
    label(5244,'Family, Gender & Life Course')
    label(5252,'Family & Relationships')
    move(5252,5244,5238)
    gender = create('Gender & Sexuality',5244)
    life = create('Life Stages',5244)
    for id in (5245,5248,5249,5250,5253):
        move(id,gender,5244)
    label(5247,'Lifestyle')
    label(5249,'Gender Roles')
    label(5250,'Sexuality')
    for id in (12281,12282,12283,12284,12294):
        move(id,life,5252)
    move(5496,life,5244)
    merge(52559,52556)
    label(52556,'By Region')
    label(52557,'North America')
    label(52558,'United States')
    move(52557,52556,5252)
    move(52558,52557,5252)
    label(52555,'History')
    label(11801,'Studies')
    label(11802,'Research & Demography')
    label(11803,'Policy & Children’s Rights')
    label(12280,'Size')
    for id in (5504,5510,5522):
        move(id,5462,5664)

    # Penology encompasses punishment theory as well as prisons/corrections.
    move(5325,5463)
    move(5465,5325,5463)
    for id in (52813,52814,52816,52817):
        move(id,5325,5465)

    # Reuse existing psychology groups. Distinct interdisciplinary fields remain.
    for id in (10863,10896,10913,10975,10982,11212):
        move(id,5414,5381)
    move(11147,5652,5381)
    move(5401,5653,5652)
    for id in (10808,10845,11014,11056):
        move(id,5653,5381)
    move(11019,5385,5381)
    move(11005,10808,5381)
    professional = create('Professional Practice',5381)
    for id in (10895,10927,10968,11130,11199):
        move(id,professional,5381)
    for id in (7018,7688,9977,10805,5390):
        move(id,5389,5381)
    move(10829,5456,5381)
    move(11170,5456,5381)
    for id in (5400,5411,5649,10902):
        move(id,5518,5381)
    move(10833,5402,5381)
    # Applied organizational psychology has no reason to duplicate a Cognition route.
    db.execute('DELETE FROM concept_parent WHERE concept_id=5410 AND parent_concept_id=5389')
    merge(12873,5389)
    labels = {
        5383:'Animal & Comparative',5384:'Applied',5386:'Character',5391:'Developmental',
        5401:'Experimental',5402:'Forensic',5407:'History',5410:'Work & Organizational',
        5426:'Foundations',5429:'Physiological',5433:'Sexual Behavior',
        5518:'Social',5649:'Social Class',5399:'Essays',5654:'Historical Methods',
        10808:'Writing & Communication',10829:'Pattern Perception',10833:'Evidence',
        10863:'Cognitive',10895:'Ethics',10896:'Positive',10902:'Influence',
        10913:'Evolutionary',10927:'Licensure & Certification',10968:'Racism',
        10975:'Discursive',10982:'Feminist',11005:'Literature',11014:'Biographical Methods',
        11058:'Values & Meaning',11130:'Vocational Guidance',11147:'Mind & Body',
        11199:'Practice & Economics',11212:'Interbehavioral',
        12259:'History & Schools',12260:'Theory',13838:'Reference & Institutions',
        13839:'Relations to Other Fields',5261:'Support Services',5235:'Land Use',
    }
    for id,text in labels.items():
        label(id,text)

    methods = create('Methods & Reference',5055)
    for id,text in [(5498,'Essays'),(5509,'Methods'),(12245,'Reference'),(12246,'Research')]:
        move(id,methods,5055)
        label(id,text)
    merge(52939,12246)
    label(52940,'United States')

    # A printed selector is an exact string, not necessarily an operational
    # numeric range. Materialize reviewed outline scopes and their existing
    # narrower printed child scopes, so the broader ranges do not swallow them.
    for id,lo,hi in [(5058,'GN301','GN674'),(5069,'GN49','GN298'),(5071,'GN700','GN890'),
                     (5389,'BF309','BF499'),(5073,'HT601','HT1445'),(5077,'HT101','HT395'),
                     (5325,'HV8301','HV9920.7'),(5465,'HV9441','HV9920.7'),
                     (5059,'GN406','GN517'),(12299,'HT101','HT160.9'),
                     (12240,'HT851','HT1445'),(9977,'BF441','BF449.5'),
                     (12979,'GN303','GN304'),(12979,'GN307.5','GN307.7'),
                     (12979,'GN307.8','GN307.82'),(12979,'GN308','GN308.3')]:
        assert not db.execute('SELECT 1 FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi)).fetchone()
        db.execute('INSERT INTO lcc_range VALUES(?,?,?)',(lo,hi,id))
        manifest['added_matching_ranges'].append([id,lo,hi])
    # The full printed penology heading belongs on the broader parent too.
    assert db.execute("UPDATE lcc_selector SET concept_id=5325 WHERE concept_id=5465 AND selector='HV8301-9920.7'").rowcount == 1

    # Prune only contained same-destination numeric ranges. Runtime before/after
    # audits independently check precedence; never bridge disjoint intervals.
    def number(code):
        m = re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)',code)
        return (m[1],Decimal(m[2])) if m else None
    for id in (10805,5413,5459,5461,5456,5253,13274,12730,5472):
        ranges = db.execute('SELECT start_code,end_code FROM lcc_range WHERE concept_id=?',(id,)).fetchall()
        for lo,hi in ranges:
            a,b=number(lo),number(hi)
            if not a or not b or a[0]!=b[0]:
                continue
            for lower,upper in ranges:
                x,y=number(lower),number(upper)
                if (lo,hi)!=(lower,upper) and x and y and x[0]==a[0]==y[0] and x[1]<=a[1] and b[1]<=y[1]:
                    db.execute('DELETE FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi))
                    manifest['removed_contained_ranges'].append([id,lo,hi,lower,upper])
                    break
    return manifest


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--apply-from',type=Path)
    args=p.parse_args()
    if args.apply_from:
        folder=args.apply_from
        with sqlite3.connect(folder/'before.sqlite3') as before, sqlite3.connect(folder/'after.sqlite3') as after, sqlite3.connect(DATABASE) as live:
            live.execute('PRAGMA foreign_keys=ON')
            live.execute('BEGIN IMMEDIATE')
            assert snapshot(live)==snapshot(before),'Concurrent taxonomy changes: regenerate plan'
            improve(live)
            validate(live)
            assert snapshot(live)==snapshot(after),'Plan differs from reviewed snapshot'
        print('Applied reviewed Social Sciences corrections')
        return
    folder=Path(tempfile.mkdtemp(prefix='social-sciences-review-',dir=ROOT/'tmp'))
    with sqlite3.connect(f'file:{DATABASE}?mode=ro',uri=True) as source:
        for name in ('before','after'):
            with sqlite3.connect(folder/f'{name}.sqlite3') as copy:
                source.backup(copy)
    with sqlite3.connect(folder/'before.sqlite3') as before, sqlite3.connect(folder/'after.sqlite3') as after:
        after.execute('PRAGMA foreign_keys=ON')
        manifest=improve(after)
        validate(after)
        old,new=snapshot(before),snapshot(after)
        manifest['changes']={t:{'removed':sorted(set(old[t])-set(new[t])),'added':sorted(set(new[t])-set(old[t]))} for t in baseline.TABLES}
    (folder/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(folder)


if __name__=='__main__':
    main()
