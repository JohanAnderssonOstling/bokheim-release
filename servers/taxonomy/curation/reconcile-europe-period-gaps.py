#!/usr/bin/env python3
import re, sqlite3, sys
from pathlib import Path

BASE=Path('/home/johan/Hem/Programmering/RustroverProjects/Html-Prototype/shared/subject-projection/data')
U=BASE/'unified-taxonomy-v2.sqlite3'; M=BASE/'master-taxonomy-v2.sqlite3'
ROOT_LABELS={'Albania','Austria','Belgium','Bulgaria','Czechoslovakia','Denmark','East Germany, 1949–1990','England','Estonia','Finland','France','Germany','Hungary','Iceland','Ireland','Latvia','Lithuania','Luxembourg','Malta','Modern Greece','Netherlands','Norway','Poland','Portugal','Prussia','Romania','Scotland','Slovakia','Spain','Sweden','Switzerland','Turkey','Ukraine','Wales','West Germany, 1949–1990','Yugoslavia & Successor States'}
BAD=re.compile(r'foreign and general|political history|social life|military history|special events|historiography|historical geography|local history|antiquities|ethnograph|place names|description and travel|\brevolution|\brevolt|\bwar of\b|\buprising|\bpeace of|\binvasion|\binterregnum|\bking\b|\bqueen\b|\bemperor\b|\bottokar|\bkarl\b|\boskar\b|\bharald\b|\bmagnus\b|\bsigurd\b',re.I)
YEAR=re.compile(r'(?<!\d)(\d{3,4})(?!\d)')
CENT=re.compile(r'(\d{1,2})(?:st|nd|rd|th) century',re.I)
def interval(label):
    if re.search(r'\bBC\b|\bBCE\b',label,re.I): return None
    ys=[int(x) for x in YEAR.findall(label)]
    cm=CENT.search(label)
    if cm:
        n=int(cm.group(1)); return ((n-1)*100,n*100-1)
    if len(ys)>=2: return (min(ys[0],ys[1]),max(ys[0],ys[1]))
    if len(ys)==1:
        y=ys[0]
        if re.search(r'\b(to|through)\b',label,re.I): return (-1000,y)
        if re.search(r'(?:-|–)\s*(?:present)?\s*$',label,re.I): return (y,2100)
    return None
def years(iv): return set(range(max(-1000,iv[0]),min(2100,iv[1])+1))

u=sqlite3.connect(U);m=sqlite3.connect(M);u.row_factory=m.row_factory=sqlite3.Row
europe={r[0] for r in u.execute("WITH RECURSIVE e(id) AS (SELECT concept_id FROM concept WHERE preferred_label='Europe' UNION SELECT p.concept_id FROM e JOIN concept_parent p ON p.parent_concept_id=e.id) SELECT id FROM e")}
roots=[]
for row in u.execute("select concept_id,preferred_label from concept"):
    if row['concept_id'] in europe and row['preferred_label'] in ROOT_LABELS:
        bp=m.execute("select p.concept_id from concept_parent p join concept c on c.concept_id=p.concept_id where p.parent_concept_id=? and lower(c.preferred_label)='by period'",(row['concept_id'],)).fetchone()
        if bp: roots.append((row['preferred_label'],row['concept_id'],bp[0]))

proposals=[]; fallbacks=[]
for name,rid,bpid in sorted(roots):
    current=set()
    for r in u.execute("WITH RECURSIVE t(id) AS (SELECT concept_id FROM concept_parent WHERE parent_concept_id=? UNION SELECT p.concept_id FROM t JOIN concept_parent p ON p.parent_concept_id=t.id) SELECT DISTINCT c.preferred_label FROM t JOIN concept c ON c.concept_id=t.id WHERE EXISTS(SELECT 1 FROM source_selector s WHERE s.concept_id=t.id)",(rid,)):
        iv=interval(r[0]); current |= years(iv) if iv else set()
    q="""WITH RECURSIVE t(id,depth) AS (SELECT concept_id,1 FROM concept_parent WHERE parent_concept_id=? UNION ALL SELECT p.concept_id,t.depth+1 FROM t JOIN concept_parent p ON p.parent_concept_id=t.id) SELECT c.concept_id,c.preferred_label,t.depth FROM t JOIN concept c ON c.concept_id=t.id WHERE EXISTS(SELECT 1 FROM source_selector s WHERE s.concept_id=t.id)"""
    cand=[]
    for r in m.execute(q,(bpid,)):
        iv=interval(r['preferred_label'])
        if not iv or BAD.search(r['preferred_label']) or r['depth']>3: continue
        has_period_kids=m.execute("select 1 from concept_parent p join source_selector s on s.concept_id=p.concept_id where p.parent_concept_id=? limit 1",(r['concept_id'],)).fetchone()
        cand.append((iv[1]-iv[0],r,iv,bool(has_period_kids)))
    for _,r,iv,aggregate in sorted(cand,key=lambda x:(x[0],x[1]['preferred_label'])):
        if u.execute("select 1 from concept_parent where concept_id=?",(r['concept_id'],)).fetchone(): continue
        missing=years(iv)-current
        if len(missing)<2: continue
        if aggregate:
            fallbacks.append((name,rid,r['concept_id'],r['preferred_label'],iv,len(missing)))
            continue
        proposals.append((name,rid,r['concept_id'],r['preferred_label'],iv,len(missing)))
        current |= years(iv)

# Semantic review: interval arithmetic deliberately over-reports when an
# existing descriptive label has no years. Only these candidates are genuine
# chronological gaps rather than rulers, events, or redundant aggregates.
REVIEWED_ADD={
 37270,
 35816,
 34939,
 37288,
 36943,
 33904,
 11566,
 35641,
 11560,
 37245,
 37208,
 33798,
 11561,
 11562,
 11563,
 11564,
 11565,
}
promoted=[p for p in fallbacks if p[2] in REVIEWED_ADD]
fallbacks=[p for p in fallbacks if p[2] not in REVIEWED_ADD]
proposals=[p for p in proposals if p[2] in REVIEWED_ADD]+promoted
for p in proposals: print('ADD','|'.join(map(str,p)))
for p in fallbacks: print('FALLBACK','|'.join(map(str,p)))
print('ROOTS',len(roots),'ADD',len(proposals),'FALLBACK',len(fallbacks))

if '--apply' in sys.argv:
  with u:
    for name,rid,cid,label,iv,_ in proposals:
      shown=label
      if re.fullmatch(r'\s*\d{3,4}\s*[-–]\s*(?:\d{3,4})?\s*',label): shown=f'{name}, {label.strip()}'
      u.execute('insert or ignore into concept(concept_id,preferred_label) values(?,?)',(cid,shown))
      ordinal=u.execute('select coalesce(max(ordinal)+1,0) from concept_parent where concept_id=?',(cid,)).fetchone()[0]
      u.execute('insert or ignore into concept_parent(concept_id,parent_concept_id,ordinal) values(?,?,?)',(cid,rid,ordinal))
      for s in m.execute('select system_id,selector from source_selector where concept_id=?',(cid,)):
        u.execute('delete from source_selector where concept_id=? and system_id=? and selector=?',(rid,s['system_id'],s['selector']))
        u.execute('insert or ignore into source_selector(concept_id,system_id,selector) values(?,?,?)',(cid,s['system_id'],s['selector']))
    for name,rid,cid,label,iv,_ in fallbacks:
      for s in m.execute('select system_id,selector from source_selector where concept_id=?',(cid,)):
        u.execute('insert or ignore into source_selector(concept_id,system_id,selector) values(?,?,?)',(rid,s['system_id'],s['selector']))
  print('APPLIED')
