#!/usr/bin/env python3
"""Reviewed Politics & Government corrections; preview by default.

Creates before/after snapshots and an explicit change manifest. --apply-from
applies a previously reviewed snapshot only if the live tables still match.
"""
import argparse
from decimal import Decimal
import json
from pathlib import Path
import re
import sqlite3
import tempfile

ROOT = Path(__file__).resolve().parents[3]
DATABASE = ROOT / 'shared/subject-projection/data/unified-taxonomy-v2.sqlite3'
TABLES = ('concept', 'concept_parent', 'source_label', 'lcc_selector', 'lcc_range', 'bisac_selector', 'ddc_selector', 'taxonomy_meta')


def snapshot(db):
    return {t: sorted(db.execute(f'SELECT * FROM {t}').fetchall()) for t in TABLES}


def improve(db):
    merges, created, removed_ranges = {}, {}, []

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

    def create(name, parent):
        id = db.execute('INSERT INTO concept(preferred_label) VALUES(?)', (name,)).lastrowid
        created[name] = id
        link(id, parent)
        return id

    def merge(old, new, keep_parents=False):
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
        for table in ('ddc_selector', 'lcc_range'):
            db.execute(f'UPDATE {table} SET concept_id=? WHERE concept_id=?', (new, old))
        for (text,) in db.execute('SELECT label FROM source_label WHERE concept_id=? ORDER BY ordinal', (old,)).fetchall():
            ordinal = db.execute('SELECT coalesce(max(ordinal),-1)+1 FROM source_label WHERE concept_id=?', (new,)).fetchone()[0]
            db.execute('INSERT OR IGNORE INTO source_label VALUES(?,?,?)', (new, text, ordinal))
        db.execute('DELETE FROM source_label WHERE concept_id=?', (old,))
        db.execute('DELETE FROM concept_parent WHERE concept_id=? OR parent_concept_id=?', (old, old))
        db.execute('DELETE FROM concept WHERE concept_id=?', (old,))
        merges[old] = new

    # Clear misassignment: TS1060-1070 = furs (LC T outline).
    furs = create('Fur Manufacturing', 440)
    assert db.execute("UPDATE lcc_range SET concept_id=? WHERE start_code='TS1060' AND end_code='TS1070' AND concept_id=13507", (furs,)).rowcount == 1

    # Split Atlantic/Indian islands from Oceania, including exact printed ranges.
    islands = create('Atlantic & Indian Ocean Islands', 5359)
    db.execute("UPDATE lcc_range SET concept_id=? WHERE concept_id=13380 AND start_code='JS7820'", (islands,))
    for (selector,) in db.execute('SELECT selector FROM lcc_selector WHERE concept_id=13380').fetchall():
        match = re.match(r'JS(\d+)', selector)
        if match and 7820 <= int(match[1]) < 8000:
            db.execute('UPDATE lcc_selector SET concept_id=? WHERE concept_id=13380 AND selector=?', (islands, selector))

    # JZ angle-bracket entries are references to KZ, not active JZ classes.
    # Preserve legacy JZ numbers on their broader existing Law destinations,
    # not as independent pseudo-subjects. Active JZ6530 is humanitarian aspects.
    db.execute("DELETE FROM lcc_range WHERE concept_id=13779 AND start_code='JZ6440'")
    db.execute("INSERT INTO lcc_range VALUES('JZ6440','JZ6530',13233)")
    humanitarian = create('Humanitarian Aspects of War', 5694)
    db.execute("INSERT INTO lcc_range VALUES('JZ6530','JZ6530',?)", (humanitarian,))
    merge(13775, 5300)
    merge(13776, 5300)
    merge(13779, 13233)
    db.execute("UPDATE lcc_range SET concept_id=5548 WHERE start_code='K3150' AND end_code='K3370' AND concept_id=5335")
    # Constitutions keeps POL022000; no invented narrow LCC substitute.

    # One geographical hierarchy, combining BISAC regional politics with LCC
    # government coverage. Preserve broader and multi-country scopes.
    region = db.execute("SELECT c.concept_id FROM concept c JOIN concept_parent p USING(concept_id) WHERE preferred_label='By Region' AND parent_concept_id=5326").fetchone()
    region = region[0] if region else create('By Region', 5326)
    for old, new in ((11786,11884),(11787,5367),(11789,11918),(11791,5368),(11792,11898),(11793,11609)):
        merge(old, new)
    for id, name in ((11884,'Africa'),(5367,'Asia'),(5368,'Europe'),(11917,'Americas'),(11788,'Oceania'),(11885,'Atlantic Ocean Islands')):
        label(id, name)
        move(id, region)
    for id in (11923,11790):
        move(id,11917)
    label(11790,'Caribbean & Latin America')
    for id in (5327,11918):
        move(id,11923)
    for id in (11919,11921):
        move(id,11790)
    for id in (11920,11922,11924):
        move(id,11921)
    for id in (11886,11901):
        move(id,11788)
    asian = (11887,11888,11889,11891,11893,11897,11898,11900,11902,11904,11905)
    for id in asian:
        move(id,5367)
    for id in (11890,11892,11894,11895,11896,11899,11906,11907,11908):
        move(id,11891)
    # LC groups Thailand under East Asia; use Southeast Asia for navigation.
    move(11907,11904)
    move(11903,11904)
    move(13509,13377)
    merge(11596,5368)
    merge(13475,13377)

    merge(12529,5338,True)
    merge(12531,5330,True)
    merge(13819,5370)
    merge(5370,5690)
    merge(5340,5690)
    # Keep JA separate: the broad theory heading previously also covered JA.
    db.execute("UPDATE lcc_range SET start_code='JC' WHERE concept_id=5690 AND start_code='JA' AND end_code='JC'")
    move(5369,5326,5690)
    for child, parent in ((11821,11762),(11822,11762),(11848,11764),(11849,11764),(11846,11766),(11847,11766)):
        move(child,parent,5690)
    for id in range(11812,11821):
        move(id,11761,5690)
    # General ideologies is useful as the common parent for all named ideologies.
    label(5364,'Ideologies')
    for id in (11759,11760,11761,11762,11764,11765,11766,11767):
        move(id,5364,5690)
    merge(5365,5695)
    merge(5380,5326)
    merge(12894,5358)
    label(5695,'Political Process, Media & Opinion')
    move(5333,5695,5697)
    label(5333,'Commentary & Opinion')
    label(5351,'Relations to Other Subjects')

    state = create('State & Civil Society',5696)
    db.execute("UPDATE ddc_selector SET concept_id=? WHERE selector='322' AND concept_id=5696",(state,))
    move(5375,state,5696)
    label(5374,'Public Policy')
    for id in range(11768,11782):
        move(id,5374,5696)
    # Remove repeated context, not meaningful scope qualifiers.
    labels = {
        5327:'United States', 5369:'Political Science', 11761:'Socialism & Communism',
        11812:'Africa',11813:'Americas',11814:'Asia-Pacific',11815:'Europe',
        11816:'Middle East & Central Asia',11817:'Oceania',11818:'Russia & Former Soviet Sphere',
        11819:'Society & Culture',11820:'Utopian',52854:'Periodicals',52855:'History',
        52856:'Scientific Socialism & Communism',52857:'Reference Works',
        11886:'Australasia',11887:'Bangladesh',11888:'Bhutan',11889:'Central Asia & Siberia',
        11890:'China',11891:'East Asia',11892:'Hong Kong',11893:'India',11894:'Japan',
        11895:'Korea',11896:'Macau',11897:'Maldives',11898:'Middle East & Caucasus',
        11899:'Mongolia',11900:'Nepal',11901:'Pacific Islands',11902:'Pakistan',
        11903:'Philippines',11904:'Southeast Asia',11905:'Sri Lanka',11906:'Taiwan',
        11907:'Thailand',11908:'Tibetan Government-in-Exile',11918:'Canada',
        11919:'Caribbean',11920:'Central America',11921:'Latin America',11922:'Mexico',
        11923:'North America',11924:'South America',52946:'Singapore',
        11598:'Britain',11599:'Ireland',11600:'Austria',11601:'Hungary',11602:'Czech Republic & Czechoslovakia',
        11603:'France',11604:'Germany',11605:'Greece',11606:'Italy',11607:'Netherlands',
        11608:'Belgium',11609:'Russia & Former Soviet Republics',11610:'Ukraine',11611:'Belarus',
        11612:'Moldova',11613:'Russian Federation',11614:'Latvia',11615:'Poland',11616:'Northern Europe',
        11617:'Denmark',11618:'Greenland',11619:'Iceland',11620:'Finland',11621:'Norway',
        11622:'Sweden',11623:'Spain',11624:'Portugal',11625:'Switzerland',11626:'Balkan States',
        11627:'Albania',11628:'Bulgaria',11629:'Romania',11630:'Yugoslavia',11631:'Other Balkan States',
        12185:'Estonia',12186:'Lithuania',12187:'Slovakia',
        13368:'Reference',13369:'Institutions & Politics',13370:'United States',13371:'Municipal',
        13372:'Other Local Government',13374:'Canada',13375:'Caribbean',13376:'Latin America',
        13377:'Europe',13378:'Asia',13379:'Africa',13381:'Polar Regions',
        13476:'European Community Countries',13477:'Eastern Europe',13478:'Britain & England',13479:'Ireland',
        13480:'Austria',13481:'Czech Republic & Czechoslovakia',13482:'Slovakia',13483:'Liechtenstein',
        13484:'France',13485:'Greece',13486:'Italy',13487:'Malta',13488:'Benelux',
        13489:'Russia & Former Soviet Republics',13490:'Armenia',13491:'Azerbaijan',13492:'Belarus',
        13493:'Georgia',13494:'Moldova',13495:'Ukraine',13496:'Russian Federation',
        13497:'Estonia',13498:'Latvia',13499:'Lithuania',13500:'Poland',13501:'Scandinavia',
        13502:'Finland',13503:'Spain',13504:'Portugal',13505:'Switzerland',13506:'Hungary',13509:'Germany',
        52801:'English-Speaking Africa',52802:'South Africa',52803:'Southern & Central Africa',
        52804:'Zambia',52805:'West Africa',52806:'Indian Ocean Islands',52807:'North Africa',
        52808:'Algeria',52809:'French-Speaking Africa',52810:'Egypt',52947:'German East Africa',
        52866:'Confederation & Empire, 1867–1918',52663:'Crown',52664:'Executive Branch',52665:'Civil Service',
        52666:'Parliament',52669:'Judiciary',52671:'Election Law',
        11768:'Agriculture & Food',11769:'Communication',11770:'Culture',11771:'Energy',
        11772:'Environment',11773:'Health Care',11774:'Immigration',11775:'Military',
        11777:'Science & Technology',11778:'Social Policy',11779:'Social Security',11781:'Social Services & Welfare',
        13773:'Reference',5353:'Societies',5354:'Sources',5350:'Non-Military Coercion',
        5355:'State Territory & Its Parts',13836:'Government & History',13837:'Confederate States',
    }
    for id,text in labels.items():
        label(id,text)
    for id,text in db.execute("SELECT concept_id,preferred_label FROM concept WHERE concept_id BETWEEN 52884 AND 52900").fetchall():
        label(id,re.sub(r'^Migration in (?:the )?', '', text))
    for id,text in ((12444,'Americas'),(12445,'Europe'),(12446,'Asia'),(12447,'Africa'),
                    (12448,'Atlantic & Indian Ocean Islands'),(12449,'Australia & Pacific'),(12450,'Polar Regions & Developing World')):
        label(id,text)

    # Only remove same-destination contained ranges, never bridge gaps.
    # Full matcher audit still checks narrower-range precedence and recovery.
    def numeric(code):
        m = re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)',code)
        return (m[1],Decimal(m[2])) if m else None
    for id in (5344,5350,5354,5355,5356,5357,5690):
        ranges=db.execute('SELECT start_code,end_code FROM lcc_range WHERE concept_id=?',(id,)).fetchall()
        for lo,hi in ranges:
            a,b=numeric(lo),numeric(hi)
            if not a or not b or a[0]!=b[0]:
                continue
            for lower,upper in ranges:
                x,y=numeric(lower),numeric(upper)
                if (lo,hi)!=(lower,upper) and x and y and x[0]==a[0]==y[0] and x[1]<=a[1] and b[1]<=y[1]:
                    db.execute('DELETE FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi))
                    removed_ranges.append([id,lo,hi,lower,upper])
                    break
    return {'merges':merges,'created':created,'removed_contained_ranges':removed_ranges}


def validate(db):
    assert db.execute('PRAGMA integrity_check').fetchone()==('ok',)
    assert not db.execute('PRAGMA foreign_key_check').fetchall()
    edges=db.execute('SELECT concept_id,parent_concept_id FROM concept_parent').fetchall()
    parents={}
    for child,parent in edges:
        parents.setdefault(child,[]).append(parent)
    done=set()
    def visit(id,active):
        assert id not in active, f'cycle at {id}'
        if id in done:return
        for parent in parents.get(id,[]):visit(parent,active|{id})
        done.add(id)
    for id in parents:visit(id,set())


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply-from',type=Path)
    args=parser.parse_args()
    if args.apply_from:
        folder=args.apply_from
        with sqlite3.connect(folder/'before.sqlite3') as before, sqlite3.connect(folder/'after.sqlite3') as after, sqlite3.connect(DATABASE) as live:
            live.execute('PRAGMA foreign_keys=ON')
            live.execute('BEGIN IMMEDIATE')
            assert snapshot(live)==snapshot(before),'Concurrent taxonomy change: regenerate plan'
            improve(live)
            validate(live)
            assert snapshot(live)==snapshot(after),'Plan differs from reviewed snapshot'
        print('Applied reviewed politics corrections')
        return
    folder=Path(tempfile.mkdtemp(prefix='politics-review-',dir=ROOT/'tmp'))
    with sqlite3.connect(f'file:{DATABASE}?mode=ro',uri=True) as source:
        for name in ('before','after'):
            with sqlite3.connect(folder/f'{name}.sqlite3') as copy:source.backup(copy)
    with sqlite3.connect(folder/'after.sqlite3') as after, sqlite3.connect(folder/'before.sqlite3') as before:
        after.execute('PRAGMA foreign_keys=ON')
        manifest=improve(after)
        validate(after)
        old,new=snapshot(before),snapshot(after)
        manifest['changes']={t:{'removed':sorted(set(old[t])-set(new[t])),'added':sorted(set(new[t])-set(old[t]))} for t in TABLES}
    (folder/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(folder)


if __name__=='__main__':main()
