#!/usr/bin/env python3
"""Build a staging classification index; never activate it as live metadata.

Raw classifier strings and complete evidence are retained. Inferred topic values
are exposed in a separate view, never promoted to exact book classifications.
"""
import argparse,importlib.util,json,sqlite3,subprocess
from pathlib import Path
import orjson

def load(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
bundle=load('bundle','extract-wikidata-complete.py')
classifiers=load('classifiers','wikidata-classifiers.py')

SCHEMA='''
CREATE TABLE IF NOT EXISTS classification(entity TEXT,property TEXT,scheme TEXT,scope TEXT,value TEXT,statement TEXT,
 PRIMARY KEY(entity,property,statement));
CREATE TABLE IF NOT EXISTS isbn(isbn13 TEXT,entity TEXT,PRIMARY KEY(isbn13,entity));
CREATE TABLE IF NOT EXISTS topic_link(entity TEXT,topic TEXT,statement TEXT,PRIMARY KEY(entity,topic,statement));
CREATE TABLE IF NOT EXISTS completed_chunk(path TEXT PRIMARY KEY,sha256 TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE VIEW IF NOT EXISTS isbn_classification_direct AS
 SELECT i.isbn13,c.*,0 AS inferred,'wikidata:direct' AS method FROM isbn i JOIN classification c ON c.entity=i.entity;
CREATE VIEW IF NOT EXISTS isbn_classification_inferred AS
 SELECT i.isbn13,i.entity AS book_entity,c.entity AS source_topic,c.property,c.scheme,c.value,c.statement,
 l.statement AS topic_link,1 AS inferred,'wikidata:topic:P921' AS method
 FROM isbn i JOIN topic_link l ON l.entity=i.entity JOIN classification c ON c.entity=l.topic WHERE c.scope='topic';
'''

def canonical_isbn(value):
    if not isinstance(value,str):return None
    s=''.join(c for c in value if c not in '- ')
    if len(s)==10 and s[:9].isascii() and s[:9].isdigit() and (s[-1].isdigit() and s[-1].isascii() or s[-1] in 'Xx'):
        digits=[int(c) for c in s[:9]]+[10 if s[-1] in 'Xx' else int(s[-1])]
        if sum((10-i)*n for i,n in enumerate(digits))%11:return None
        s='978'+s[:9];s+=str((-sum(int(n)*(1 if i%2==0 else 3) for i,n in enumerate(s)))%10)
    if len(s)!=13 or not s.isascii() or not s.isdigit() or s[:3] not in ('978','979'):return None
    return s if sum(int(n)*(1 if i%2==0 else 3) for i,n in enumerate(s))%10==0 else None


def insert(c,record,registry):
    entity=record['id'];claims=record.get('claims',{})
    for e in classifiers.assigned(record,registry):
        c.execute('INSERT OR IGNORE INTO classification VALUES(?,?,?,?,?,?)',
                  (entity,e['property'],e['scheme'],e['property_scope'],e['value'],json.dumps(e['statement'],sort_keys=True)))
    for prop in ('P212','P957'):
        for statement in classifiers.values(claims.get(prop,[])):
            isbn=canonical_isbn(statement['mainsnak']['datavalue']['value'])
            if isbn:c.execute('INSERT OR IGNORE INTO isbn VALUES(?,?)',(isbn,entity))
    for statement in classifiers.values(claims.get('P921',[])):
        target=statement['mainsnak']['datavalue']['value']
        if isinstance(target,dict) and isinstance(target.get('id'),str):
            c.execute('INSERT OR IGNORE INTO topic_link VALUES(?,?,?)',(entity,target['id'],json.dumps(statement,sort_keys=True)))


def build(root,database):
    root=Path(root);chunks=list(bundle.completed_chunks(root))
    props=[]
    for chunk in chunks:props.extend(json.loads(chunk.with_suffix('.meta.json').read_text()).get('classifier_properties',[]))
    registry=classifiers.definitions(props)
    c=sqlite3.connect(database);c.execute('PRAGMA journal_mode=WAL');c.executescript(SCHEMA)
    signature=json.dumps(json.loads((root/'manifest.json').read_text()),sort_keys=True)
    old=c.execute("SELECT value FROM state WHERE key='source'").fetchone()
    if old and old[0]!=signature:raise ValueError('index source changed')
    c.execute("INSERT OR REPLACE INTO state VALUES('source',?)",(signature,));c.execute("INSERT OR REPLACE INTO state VALUES('status','building')");c.commit()
    tokens=[('"'+p+'"').encode() for p in list(registry)+['P212','P957','P921']]
    for chunk in chunks:
        meta=json.loads(chunk.with_suffix('.meta.json').read_text())
        key=str(chunk.relative_to(root));previous=c.execute('SELECT sha256 FROM completed_chunk WHERE path=?',(key,)).fetchone()
        if previous:
            if previous[0]!=meta['sha256']:raise ValueError('indexed chunk changed')
            continue
        p=subprocess.Popen(['zstd','-q','-dc',str(chunk)],stdout=subprocess.PIPE)
        try:
            with c:
                for line in p.stdout:
                    if any(t in line for t in tokens):insert(c,orjson.loads(line),registry)
                if p.wait()!=0:raise RuntimeError('zstd failed')
                c.execute('INSERT INTO completed_chunk VALUES(?,?)',(key,meta['sha256']))
            print(json.dumps({'chunk':key,'status':'indexed'}),flush=True)
        finally:
            if p.poll() is None:p.kill();p.wait()
            p.stdout.close()
    c.execute("INSERT OR REPLACE INTO state VALUES('status','complete')");c.commit();c.close()

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('bundle');p.add_argument('database');a=p.parse_args();build(a.bundle,a.database)
