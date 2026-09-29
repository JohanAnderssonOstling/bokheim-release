#!/usr/bin/env python3
"""Run the optimized production comparison on snapshots and observed notations."""
import argparse
import json
from pathlib import Path
import sqlite3
import subprocess

ROOT=Path(__file__).resolve().parents[3]


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('folder',type=Path)
    p.add_argument('--full',action='store_true')
    args=p.parse_args()
    folder=args.folder.resolve()
    prefix='full' if args.full else 'selectors'
    audited=folder/'matcher-audited.sqlite3'
    if not audited.exists():
        with sqlite3.connect(folder/'after.sqlite3') as source, sqlite3.connect(audited) as target:
            source.backup(target)
    with (folder/f'{prefix}-changes.tsv').open('w') as output:
        child=subprocess.Popen([str(ROOT/'tmp/desktop-release-build/taxonomy_compare_stream'),str(folder/'before.sqlite3'),str(folder/'after.sqlite3'),str(folder/f'{prefix}-counts.tsv')],stdin=subprocess.PIPE,stdout=output,text=True)
        def send(system,code,uses=0):
            child.stdin.write(f'{system}\t{uses}\t{code.encode().hex()}\n')
        if args.full:
            cache=Path.home()/'.cache/bokheim/openlibrary-code-usage.sqlite3'
            with sqlite3.connect(f'file:{cache}?mode=ro',uri=True) as db:
                assert db.execute("SELECT value FROM cache_metadata WHERE key='normalization_version'").fetchone()==('2',)
                for code,uses in db.execute('SELECT notation,uses FROM code_usage WHERE scheme=2'):
                    send('lcc',code,uses)
        else:
            codes=set()
            for name in ('before','after'):
                with sqlite3.connect(folder/f'{name}.sqlite3') as db:
                    for system in ('lcc','bisac','ddc'):
                        codes.update((system,r[0]) for r in db.execute(f'SELECT selector FROM {system}_selector'))
                    for lo,hi in db.execute('SELECT start_code,end_code FROM lcc_range'):
                        codes.update(('lcc',c) for c in (lo,hi,lo+'..'+hi))
            for system,code in sorted(codes):send(system,code)
        child.stdin.close()
        assert child.wait()==0
    manifest=json.loads((folder/'manifest.json').read_text())
    merges={int(k):v for k,v in manifest['merges'].items()}
    def canonical(id):
        while id in merges:id=merges[id]
        return id
    changes=[]
    for line in (folder/f'{prefix}-changes.tsv').read_text().splitlines():
        system,uses,code,old,new=line.split('\t')
        oldids=set(map(int,filter(None,old.split('|'))))
        newids=set(map(int,filter(None,new.split('|'))))
        if {canonical(i) for i in oldids}!=newids:
            changes.append({'system':system,'code':bytes.fromhex(code).decode(),'uses':int(uses),'before':sorted(oldids),'after':sorted(newids)})
    (folder/f'{prefix}-nonmerge-changes.json').write_text(json.dumps(changes,indent=2)+'\n')
    print(f'{prefix}: {len(changes)} changes beyond intentional concept merges',flush=True)


if __name__=='__main__':main()
