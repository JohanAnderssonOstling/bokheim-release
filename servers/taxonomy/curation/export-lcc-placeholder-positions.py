#!/usr/bin/env python3
"""Export explicitly defined .x positions attached to first-Cutter intervals."""
import csv,json,pathlib,re,sqlite3,collections
root=pathlib.Path(__file__).resolve().parents[3] / 'shared/subject-projection'
c=sqlite3.connect((root/'data/lcc-mdsconnect-2016.sqlite3').resolve().as_uri()+'?mode=ro',uri=True);c.row_factory=sqlite3.Row
known={r[k] for r in csv.DictReader((root/'data/lcc-2024-outline.csv').open()) for k in ['start_letters','end_letters']}
ids=set((root/'data/lcc-table-identifiers.txt').read_text().splitlines());points=collections.defaultdict(set)
for table,start,end in c.execute('SELECT table_id,start_number,end_number FROM classification_span'):
    if table and table.upper() in ids and start and re.fullmatch(r'\.x\d*',start) and (not end or end==start):points[table.upper()].add(start.upper())
pages={(r['document_id'],r['page']):r['text'].splitlines() for r in c.execute('SELECT document_id,page,text FROM reference_page')};out=set()
for r in c.execute('SELECT * FROM reference_entry WHERE table_id IS NULL'):
    lo,hi=r['start_number'],r['end_number'];m=re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)\.([A-Z]\d*)',lo or '')
    if not m or m[1].startswith('K') or m[1] not in known or hi!=m[1]+m[2]+'.Z':continue
    lines=pages.get((r['document_id'],r['page']),[])
    for i,line in enumerate(lines):
        if not line.strip().startswith(r['notation']+' '):continue
        stop=next((j for j in range(i+1,len(lines)) if re.match(r'^\s*[\[(]?\d',lines[j])),len(lines))
        for line in lines[i+1:stop]:
            point=re.match(r'^\s+\.x(\d*)\s+',line)
            if point:out.add((lo,hi,'.X'+point[1]))
            table=re.search(r'Subarrange\b.*?\bTable\s+([A-Za-z0-9-]+)',line)
            if table:
                for point in points.get(table[1].upper(),[]):out.add((lo,hi,point))
(root/'data/lcc-placeholder-positions.tsv').write_text(''.join('\t'.join(row)+'\n' for row in sorted(out)))
print(f'{len(out)} documented placeholder positions')
