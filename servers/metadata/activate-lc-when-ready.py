#!/usr/bin/env python3
import hashlib,json,pathlib,subprocess,time,traceback
P=pathlib.Path
root=P('/home/johan/data/loc-books-2016')
state=root/'activation-all-records-pending.json'
current=P('/home/johan/data/openlibrary/current.sqlite')
binary=P('/home/johan/bin/metadata-server-lc-all-records')
importer=json.loads((root/'extraction-all-records-process.json').read_text())
expected=current.resolve();checksum=hashlib.sha256(binary.read_bytes()).hexdigest()
out=P(importer['output'])
def status(**values):
    values.update(updated_at_unix=time.time(),snapshot=str(out))
    tmp=state.with_suffix('.tmp');tmp.write_text(json.dumps(values,indent=2));tmp.replace(state)
try:
    status(status='waiting_for_import',expected_current=str(expected),binary_sha256=checksum)
    deadline=time.time()+36*3600
    while not out.exists():
        if time.time()>deadline:raise RuntimeError('Import did not complete within 36 hours')
        process=P('/proc')/str(importer['pid'])/'cmdline'
        if not process.exists() or b'import-loc-lcc-v2.py' not in process.read_bytes():
            if out.exists():break
            raise RuntimeError('LC importer stopped before publishing its completed snapshot')
        time.sleep(30)
    assert current.resolve()==expected,'Active snapshot changed since deployment was scheduled'
    assert hashlib.sha256(binary.read_bytes()).hexdigest()==checksum,'Staged release binary changed'
    status(status='validating_and_deploying')
    subprocess.run(['python3','-u',str(root/'deploy-lc-snapshot.py'),str(out),'--full'],check=True)
    status(status='deployed')
except BaseException as e:
    status(status='failed',error=str(e));traceback.print_exc();raise
