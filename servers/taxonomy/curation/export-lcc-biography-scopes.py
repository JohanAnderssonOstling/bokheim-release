#!/usr/bin/env python3
"""Export first-Cutter DS individual-biography scopes, not historical table aliases."""
import json,re,sqlite3
from pathlib import Path
root=Path(__file__).resolve().parents[3] / 'shared/subject-projection'
c=sqlite3.connect(f'file:{root}/data/lcc-mdsconnect-2016.sqlite3?mode=ro',uri=True)
rows=set()
for start,end,caption,hierarchy in c.execute("SELECT start_number,end_number,caption,hierarchy_json FROM reference_entry WHERE document_id=7 AND caption='Individual, A-Z'"):
    m=re.fullmatch(r'(DS\d+(?:\.\d+)?)\.([A-Z]\d*)',start or '')
    if m and end==m[1]+'.Z' and any('biograph' in h.lower() for h in json.loads(hierarchy)):
        rows.add((start,end))
assert rows and ('DS481.A2','DS481.Z') in rows
(root/'data/lcc-ds-biography-scopes.tsv').write_text(''.join(a+'\t'+b+'\n' for a,b in sorted(rows)))
print(f'Exported {len(rows)} first-Cutter DS biography scopes')
