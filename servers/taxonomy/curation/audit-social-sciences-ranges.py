#!/usr/bin/env python3
"""Isolate range simplification from intentional changes to code destinations."""
import argparse
from decimal import Decimal
import json
from pathlib import Path
import re
import sqlite3
import subprocess

ROOT=Path(__file__).resolve().parents[3]


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('folder',type=Path)
    args=p.parse_args()
    folder=args.folder.resolve()
    manifest=json.loads((folder/'manifest.json').read_text())
    restored=folder/'unsimplified.sqlite3'
    with sqlite3.connect(folder/'after.sqlite3') as source, sqlite3.connect(restored) as target:
        source.backup(target)
        for id,lo,hi,*_ in manifest['removed_contained_ranges']:
            target.execute('INSERT INTO lcc_range VALUES(?,?,?)',(lo,hi,id))
    codes=set()
    def numeric(code):
        m=re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)',code)
        return (m[1],Decimal(m[2])) if m else None
    with sqlite3.connect(restored) as db:
        codes.update(r[0] for r in db.execute('SELECT selector FROM lcc_selector'))
        for lo,hi in db.execute('SELECT start_code,end_code FROM lcc_range'):
            codes.update((lo,hi,lo+'..'+hi))
            for code in (lo,hi):
                n=numeric(code)
                if n:
                    for delta in (Decimal('-.0001'),Decimal('.0001')):
                        if n[1]+delta>=0:
                            codes.add(n[0]+str(n[1]+delta))
                    codes.add(code+'.A1')
                    codes.add(code+'.Z99')
    for id,lo,hi,*_ in manifest['removed_contained_ranges']:
        a,b=numeric(lo),numeric(hi)
        for value in range(int(a[1]*100),int(b[1]*100)+1):
            codes.add(a[0]+str(Decimal(value)/100))
    with (folder/'simplification-changes.tsv').open('w') as output:
        result=subprocess.run([str(ROOT/'tmp/desktop-release-build/taxonomy_compare_stream'),
            str(restored),str(folder/'after.sqlite3'),str(folder/'simplification-counts.tsv')],
            input=''.join('lcc\t0\t'+c.encode().hex()+'\n' for c in sorted(codes)),
            stdout=output,stderr=subprocess.PIPE,text=True,check=True)
    (folder/'simplification-audit.log').write_text(result.stderr)
    assert not (folder/'simplification-changes.tsv').stat().st_size,result.stderr
    (folder/'simplification-summary.json').write_text(json.dumps({'probes':len(codes),'changes':0,'removed_ranges':len(manifest['removed_contained_ranges'])},indent=2)+'\n')
    print(result.stderr)


if __name__=='__main__':main()
