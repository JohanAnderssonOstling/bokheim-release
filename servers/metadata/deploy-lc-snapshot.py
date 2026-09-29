#!/usr/bin/env python3
"""Validate and deploy one complete LC snapshot with rollback on failed checks."""
import argparse, json, os, pathlib, shutil, sqlite3, subprocess, time, urllib.request
P=pathlib.Path
# Snapshot activation preserves the installed server release and its configured providers.
BINARY=P('/usr/local/bin/metadata-server')
CURRENT=P('/home/johan/data/openlibrary/current.sqlite')
INCOMING=P('/var/lib/bokheim-deploy/metadata-incoming')
ROOT=P('/home/johan/data/loc-books-2016')
def request(port, endpoint, data=None):
    req=urllib.request.Request(f'http://127.0.0.1:{port}/{endpoint}',data=json.dumps(data).encode() if data is not None else None,headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req,timeout=40) as r:
        body=r.read()
    return json.loads(body) if data is not None else body.strip().decode()
def check_http(port, full, samples):
    assert request(port,'health')=='ok'
    result=request(port,'v2/classifications',{'isbns':['9780131103627']})
    assert any(c['scheme']=='library_of_congress' for r in result['results'] for m in r['matches'] for c in m['classifications']),result
    if full:
        checked=False
        for record_id,m in samples:
            if not m.get('title') or not m.get('authors'): continue
            result=request(port,'v2/edition-identities',{'queries':[{'query_id':'lc-check','title':m['title'],'authors':m['authors'],'publishers':[],'book_year':None}]})
            r=result['results'][0]
            if r.get('authority_subjects'):
                hit=r['authority_subjects']
                assert hit['provider_id']=='library_of_congress' and hit['classifications']
                assert record_id in hit['record_ids'],(record_id,hit)
                assert r.get('matched') is None
                checked=True
                print('authority_lookup_verified',record_id,m['title'],flush=True)
                break
        assert checked,'No no-ISBN authority fallback verified'
def stage(binary):
    INCOMING.mkdir()
    shutil.copy2(binary,INCOMING/'metadata-server')
    (INCOMING/'metadata-server').chmod(0o755)
def link(target):
    pending=CURRENT.with_name('current.sqlite.lc-all-records-pending')
    assert not pending.is_symlink() and not pending.exists()
    pending.symlink_to(target);os.replace(pending,CURRENT)
def deploy(candidate,full=False):
    candidate=P(candidate).resolve(strict=True)
    assert BINARY.is_file() and os.access(BINARY,os.X_OK)
    assert not INCOMING.exists(),'Another deployment has staged artifacts'
    old=CURRENT.resolve(strict=True)
    for name in ['bokheim-metadata.service']:
        assert (P('/etc/systemd/system')/name).read_bytes()==(P('/usr/local/share/bokheim-deploy')/name).read_bytes(),'Service template differs from installed unit'
    c=sqlite3.connect(candidate.as_uri()+'?mode=ro',uri=True)
    assert c.execute('select count(*),sum(records_read) from lcc_import').fetchone()==(43,10543015)
    source=sqlite3.connect(old.as_uri()+'?mode=ro',uri=True)
    assert c.execute('select schema_version,dump_date,edition_records,work_records,author_records from snapshot').fetchone()==source.execute('select schema_version,dump_date,edition_records,work_records,author_records from snapshot').fetchone(),'OL corpus differs'
    samples=[]
    if full:
        assert c.execute('select version from lc_import_version').fetchone()==(2,)
        rows=c.execute('SELECT r.lc_record_id,r.metadata FROM lc_record r WHERE EXISTS(SELECT 1 FROM lc_record_lcc l WHERE l.lc_record_id=r.lc_record_id) AND NOT EXISTS(SELECT 1 FROM lc_record_isbn i WHERE i.lc_record_id=r.lc_record_id) LIMIT 20')
        samples=[(i,json.loads(m)) for i,m in rows]
        assert samples
    c.close();source.close()
    env=dict(os.environ,BOKHEIM_METADATA_DATABASE=str(candidate),BOKHEIM_METADATA_BIND='127.0.0.1:18097',RUST_LOG='warn')
    with (ROOT/'deploy-shadow.log').open('a') as log:
        p=subprocess.Popen([str(BINARY)],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
        try:
            for _ in range(30):
                if p.poll() is not None: raise RuntimeError('Shadow server exited')
                try:
                    if request(18097,'health')=='ok':break
                except OSError:time.sleep(1)
            else:raise RuntimeError('Shadow server not ready')
            check_http(18097,full,samples)
        finally:
            p.terminate()
            try:p.wait(timeout=10)
            except subprocess.TimeoutExpired:p.kill();p.wait()
    assert CURRENT.resolve()==old,'Active snapshot changed during validation'
    backup=ROOT/('activation-all-records-'+str(time.time_ns()));backup.mkdir()
    for path in ['/usr/local/bin/metadata-server']:
        shutil.copy2(path,backup/P(path).name)
    (backup/'previous-snapshot').write_text(str(old))
    stage(BINARY)
    try:
        link(candidate)
        subprocess.run(['sudo','-n','/usr/local/sbin/bokheim-deploy-metadata'],check=True)
        check_http(8091,full,samples)
    except BaseException:
        link(old)
        if INCOMING.exists():shutil.rmtree(INCOMING)
        stage(backup/'metadata-server')
        subprocess.run(['sudo','-n','/usr/local/sbin/bokheim-deploy-metadata'],check=True)
        raise
    result=dict(status='deployed',snapshot=str(candidate),full=full,backup=str(backup),completed_at_unix=time.time())
    (ROOT/'deployment-all-records-status.json').write_text(json.dumps(result,indent=2))
    print(json.dumps(result),flush=True)
if __name__=='__main__':
    a=argparse.ArgumentParser();a.add_argument('snapshot');a.add_argument('--full',action='store_true');args=a.parse_args();deploy(args.snapshot,args.full)
