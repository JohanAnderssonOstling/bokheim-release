#!/usr/bin/env python3
"""Prepare resumable ISBN/book/author input for offline catalog ingestion.
Never edits the live catalog or the concurrent classification index.
"""
import argparse, fcntl, gzip, importlib.util, json, os, re, sqlite3, subprocess, time
from pathlib import Path
import orjson

def load(name, file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
bundle=load('author_bundle','extract-wikidata-complete.py')
classifier=load('author_classifier','index-wikidata-classifiers.py')
IDS={'P648':'openlibrary','P214':'viaf','P213':'isni','P496':'orcid','P244':'lc'}
SCHEMA=Path(__file__).with_name('wikidata-author-schema.sql').read_text()
def values(claims, prop):
    active=[s for s in claims.get(prop,[]) if s.get('rank')!='deprecated' and s.get('mainsnak',{}).get('snaktype')=='value' and 'datavalue' in s['mainsnak']]
    preferred=[s for s in active if s.get('rank')=='preferred']
    return [s['mainsnak']['datavalue']['value'] for s in preferred or active]
def qids(claims,prop):return sorted({v['id'] for v in values(claims,prop) if isinstance(v,dict) and re.fullmatch(r'Q[1-9][0-9]*',v.get('id',''))})
def strings(claims,prop):return sorted({v.strip() for v in values(claims,prop) if isinstance(v,str) and v.strip() and len(v)<=4096})
def year(claims,prop):
    years={int(v['time'][:5]) for v in values(claims,prop) if isinstance(v,dict) and v.get('precision',0)>=9 and re.match(r'^\+[0-9]{4}-',v.get('time',''))}
    return next(iter(years)) if len(years)==1 else None

def insert(c,record):
    qid=record.get('id','')
    if not re.fullmatch(r'Q[1-9][0-9]*',qid):return
    claims=record.get('claims',{})
    isbn_values={isbn for prop in ('P212','P957') for v in strings(claims,prop) if (isbn:=classifier.canonical_isbn(v))}
    authors=list(dict.fromkeys(v['id'] for v in values(claims,'P50') if isinstance(v,dict) and re.fullmatch(r'Q[1-9][0-9]*',v.get('id',''))));works=qids(claims,'P629')
    is_human='Q5' in qids(claims,'P31')
    # Keep profiles of humans and books/works, not every unrelated item.
    has_author_ids=any(p in claims for p in ('P214','P213','P496','P244')) or any(re.fullmatch(r'OL[1-9][0-9]*A',v) for v in strings(claims,'P648'))
    if is_human or has_author_ids or authors or works or isbn_values:
        labels=record.get('labels',{});name=labels.get('en',{}).get('value')
        if not name:name=next((v.get('value') for k,v in sorted(labels.items()) if v.get('value')),None)
        aliases={v.get('value') for group in record.get('aliases',{}).values() for v in group if v.get('value')}
        aliases.update(v.get('value') for v in labels.values() if v.get('value'))
        identifiers=[{'authority':authority,'value':v} for prop,authority in IDS.items() for v in strings(claims,prop)]
        description=record.get('descriptions',{}).get('en',{}).get('value')
        c.execute('INSERT OR REPLACE INTO profile VALUES(?,?,?,?,?,?,?,?)',(qid,name,json.dumps(sorted(aliases)),year(claims,'P569'),year(claims,'P570'),json.dumps(identifiers),next(iter(strings(claims,'P18')),None),description))
    c.executemany('INSERT OR IGNORE INTO isbn VALUES(?,?)',[(v,qid) for v in isbn_values])
    c.executemany('INSERT OR IGNORE INTO credit VALUES(?,?,?)',[(qid,a,i) for i,a in enumerate(authors)])
    c.executemany('INSERT OR IGNORE INTO edition_work VALUES(?,?)',[(qid,w) for w in works])
    c.executemany('INSERT OR IGNORE INTO topic_link VALUES(?,?)',[(qid,t) for t in qids(claims,'P921')])
    # Book classifications can fill fields; topic codes remain evidence.
    for prop,(scheme,scope) in classifier.classifiers.KNOWN.items():
        for notation in strings(claims,prop):
            c.execute('INSERT OR IGNORE INTO classification VALUES(?,?,?,?,?,?)',(qid,scheme,notation,prop,int(scope=='topic'),qid))

def ol_year(value):
    if not isinstance(value,str) or re.search(r'circa|about|\?|\bc\.',value,re.I):return None
    years=re.findall(r'(?<!\d)(1[0-9]{3}|20[0-9]{2})(?!\d)',value)
    return int(years[0]) if len(years)==1 else None

def build(root,authors,output,ol_dump_date):
    output=Path(output);output.parent.mkdir(parents=True,exist_ok=True)
    with open(str(output)+'.lock','a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        _build(root,authors,output,ol_dump_date)

def _build(root,authors,output,ol_dump_date):
    root=Path(root);authors=Path(authors);output=Path(output)
    output.parent.mkdir(parents=True,exist_ok=True)
    if output.exists():raise ValueError('completed output already exists')
    staging=Path(str(output)+'.building')
    c=sqlite3.connect(staging)
    try:
        c.execute('PRAGMA journal_mode=WAL');c.execute('PRAGMA cache_size=-65536');c.executescript(SCHEMA)
        signature=json.dumps({'version':1,'ol_dump_date':ol_dump_date,'bundle':json.loads((root/'manifest.json').read_text()),'authors':[str(authors.resolve()),authors.stat().st_size,authors.stat().st_mtime_ns]},sort_keys=True)
        old=c.execute("SELECT value FROM state WHERE key='source'").fetchone()
        if old and old[0]!=signature:raise ValueError('input changed; cannot resume')
        c.execute("INSERT OR REPLACE INTO state VALUES('schema_version','1')");c.execute("INSERT OR REPLACE INTO state VALUES('ol_dump_date',?)",(ol_dump_date,));c.execute("INSERT OR REPLACE INTO state VALUES('source',?)",(signature,));c.execute("INSERT OR REPLACE INTO state VALUES('status','building')");c.commit()
        print(json.dumps({"phase":"validating_chunks"}),flush=True)
        chunks=list(bundle.completed_chunks(root));started=time.monotonic()
        for i,chunk in enumerate(chunks):
            meta=json.loads(chunk.with_suffix('.meta.json').read_text());key=str(chunk.relative_to(root))
            old=c.execute('SELECT sha256 FROM completed_chunk WHERE path=?',(key,)).fetchone()
            if old:
                if old[0]!=meta['sha256']:raise ValueError('chunk changed')
                continue
            child=subprocess.Popen(['zstd','-q','-dc',str(chunk)],stdout=subprocess.PIPE)
            try:
                with c:
                    for line in child.stdout:
                        insert(c,orjson.loads(line))
                    if child.wait()!=0:raise RuntimeError('decompression failed')
                    # Chunk hashes describe compressed files in older bundles, so
                    # identity is pinned by their extraction manifest/checkpoint.
                    c.execute('INSERT INTO completed_chunk VALUES(?,?)',(key,meta['sha256']))
            finally:
                if child.poll() is None:child.kill();child.wait()
                child.stdout.close()
            print(json.dumps({'phase':'wikidata','chunks':i+1,'total_chunks':len(chunks),'elapsed_seconds':round(time.monotonic()-started)}),flush=True)
        if not c.execute("SELECT 1 FROM state WHERE key='ol_complete'").fetchone():
            with gzip.open(authors,'rb') as f:
                for n,line in enumerate(f,1):
                    record=orjson.loads(line.split(b'\t',4)[-1]);key=record.get('key','')
                    match=re.fullmatch(r'/authors/OL([1-9][0-9]*)A',key)
                    if match:
                        aliases=record.get('alternate_names',[])
                        if not isinstance(aliases,list):aliases=[]
                        aliases=[a for a in aliases if isinstance(a,str) and len(a)<=4096]
                        birth=ol_year(record.get('birth_date'));death=ol_year(record.get('death_date'))
                        if aliases or birth is not None or death is not None:
                            c.execute('INSERT OR REPLACE INTO ol_profile VALUES(?,?,?,?)',(int(match[1]),json.dumps(aliases),birth,death))
                    if n%100000==0:c.commit();print(json.dumps({'phase':'openlibrary_authors','records':n}),flush=True)
            c.execute("INSERT INTO state VALUES('ol_complete','1')");c.commit()
        c.executescript('''
        CREATE INDEX IF NOT EXISTS isbn_by_book ON isbn(book,isbn13);
        CREATE INDEX IF NOT EXISTS credit_by_author ON credit(author,book);
        CREATE INDEX IF NOT EXISTS edition_by_work ON edition_work(work,book);
        CREATE TABLE IF NOT EXISTS isbn_author(isbn13 TEXT,book TEXT,work TEXT,author TEXT,source TEXT,PRIMARY KEY(isbn13,book,work,author));
        INSERT OR IGNORE INTO isbn_author
          SELECT i.isbn13,i.book,COALESCE(w.work,i.book),c.author,c.book FROM isbn i JOIN credit c ON c.book=i.book LEFT JOIN edition_work w ON w.book=i.book;
        INSERT OR IGNORE INTO isbn_author
          SELECT i.isbn13,i.book,w.work,c.author,c.book FROM isbn i JOIN edition_work w ON w.book=i.book JOIN credit c ON c.book=w.work
          WHERE NOT EXISTS(SELECT 1 FROM credit d WHERE d.book=i.book);
        CREATE INDEX IF NOT EXISTS isbn_author_by_author ON isbn_author(author,work,isbn13);
        ANALYZE;
        ''')
        c.execute("INSERT OR REPLACE INTO state VALUES('status','complete')");c.commit()
        c.execute('PRAGMA wal_checkpoint(TRUNCATE)');c.execute('PRAGMA journal_mode=DELETE')
        c.close();os.replace(staging,output)
        print(json.dumps({'status':'complete','output':str(output)}),flush=True)
    finally:
        c.close()
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('bundle');p.add_argument('authors');p.add_argument('output');p.add_argument('--openlibrary-dump-date',required=True);a=p.parse_args();build(a.bundle,a.authors,a.output,a.openlibrary_dump_date)
