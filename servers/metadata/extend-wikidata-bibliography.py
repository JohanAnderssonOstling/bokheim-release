#!/usr/bin/env python3
"""Checkpointed bibliography/publisher backfill from the existing extracted bundle.
Run after index-wikidata-authors.py and before relational ingestion.
"""
import argparse, fcntl, json, sqlite3, subprocess
from pathlib import Path
import importlib.util
spec=importlib.util.spec_from_file_location('authors',Path(__file__).with_name('index-wikidata-authors.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
SCHEMA='''CREATE TABLE IF NOT EXISTS bibliography(book TEXT PRIMARY KEY,title TEXT,subtitle TEXT,book_year INTEGER);
CREATE TABLE IF NOT EXISTS book_publisher(book TEXT,publisher TEXT,name TEXT,PRIMARY KEY(book,publisher));
CREATE INDEX IF NOT EXISTS book_publisher_by_entity ON book_publisher(publisher);
CREATE TABLE IF NOT EXISTS bibliography_chunk(phase TEXT,path TEXT,sha256 TEXT,PRIMARY KEY(phase,path));'''
BOOK_TYPES={'Q571','Q7725634','Q47461344','Q8261','Q49084','Q49085','Q860861','Q35760'}
def title_value(claims,prop):
    vals=[v for v in m.values(claims,prop) if isinstance(v,dict) and isinstance(v.get('text'),str) and 0<len(v['text'].strip())<=4096]
    english=[v for v in vals if v.get('language')=='en']
    values={v['text'].strip() for v in english or vals}
    return next(iter(values)) if len(values)==1 else None

def insert(c,r):
    claims=r.get('claims',{});qid=r.get('id','')
    # Scientific articles are not silently imported as books merely for P50.
    direct=any(scope!='topic' and p in claims for p,(_,scope) in m.classifier.classifiers.KNOWN.items())
    if not (set(m.qids(claims,'P31')) & BOOK_TYPES or any(p in claims for p in ('P212','P957','P629')) or direct):return
    labels=r.get('labels',{});label=labels.get('en',{}).get('value') or next((v.get('value') for _,v in sorted(labels.items()) if v.get('value')),None)
    title=title_value(claims,'P1476') or label
    if not title or len(title)>4096:return
    m.insert(c,r) # Also retain profiles, direct codes and credits for ISBN-less books.
    c.execute('INSERT OR REPLACE INTO bibliography VALUES(?,?,?,?)',(qid,title,title_value(claims,'P1680'),m.year(claims,'P577')))
    c.executemany('INSERT OR IGNORE INTO book_publisher VALUES(?,?,NULL)',[(qid,p) for p in m.qids(claims,'P123')])

def extend(root,path):
    with open(str(path)+'.lock','a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        c=sqlite3.connect(path);c.execute("PRAGMA journal_mode=WAL");c.executescript(SCHEMA)
        assert c.execute("SELECT value FROM state WHERE key='status'").fetchone()==('complete',)
        if c.execute("SELECT value FROM state WHERE key='bibliography_status'").fetchone()==('complete',):return
        c.execute("INSERT OR REPLACE INTO state VALUES('bibliography_status','building')");c.commit()
        chunks=list(m.bundle.completed_chunks(Path(root)))
        for phase in ('bibliography','publishers'):
            publishers={r[0] for r in c.execute('SELECT DISTINCT publisher FROM book_publisher')} if phase=='publishers' else set()
            for n,chunk in enumerate(chunks,1):
                key=str(chunk.relative_to(root));sha=json.loads(chunk.with_suffix('.meta.json').read_text())['sha256']
                old=c.execute('SELECT sha256 FROM bibliography_chunk WHERE phase=? AND path=?',(phase,key)).fetchone()
                if old:
                    assert old==(sha,),'input chunk changed';continue
                proc=subprocess.Popen(['zstd','-q','-dc',str(chunk)],stdout=subprocess.PIPE)
                try:
                    with c:
                        for line in proc.stdout:
                            r=m.orjson.loads(line)
                            if phase=='bibliography':insert(c,r)
                            elif r.get('id') in publishers:
                                labels=r.get('labels',{});name=labels.get('en',{}).get('value') or next((v.get('value') for _,v in sorted(labels.items()) if v.get('value')),None)
                                if name and len(name)<=4096:c.execute('UPDATE book_publisher SET name=? WHERE publisher=?',(name,r['id']))
                        if proc.wait()!=0:raise RuntimeError('decompression failed')
                        c.execute('INSERT INTO bibliography_chunk VALUES(?,?,?)',(phase,key,sha))
                finally:
                    if proc.poll() is None:proc.kill();proc.wait()
                    proc.stdout.close()
                print(json.dumps(dict(phase=phase,chunks=n,total_chunks=len(chunks))),flush=True)
        c.execute("INSERT OR REPLACE INTO state VALUES('bibliography_status','complete')");c.commit();c.close()
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('bundle',type=Path);p.add_argument('input',type=Path);a=p.parse_args();extend(a.bundle,a.input)
