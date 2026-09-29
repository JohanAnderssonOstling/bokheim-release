#!/usr/bin/env python3
"""Export numeric-relative Cutter ranges for validating nested operations only."""
import pathlib,re,sqlite3
root=pathlib.Path(__file__).resolve().parents[3] / 'shared/subject-projection'
ids=set((root/'data/lcc-table-identifiers.txt').read_text().splitlines())
pattern=re.compile(r'\.x(\d+)([A-Z])(\d*)')
c=sqlite3.connect((root/'data/lcc-mdsconnect-2016.sqlite3').resolve().as_uri()+'?mode=ro',uri=True);rows=set()
for table,start,end in c.execute('SELECT table_id,start_number,end_number FROM classification_span'):
    end=end or start
    if not table or table.upper() not in ids or table.upper().startswith('K'):continue
    a=pattern.fullmatch(start or '');b=pattern.fullmatch(end or '')
    if a and b and a[1]==b[1] and (a[2],a[3].rstrip('0')) <= (b[2],b[3].rstrip('0')):
        rows.add((table.upper(),start.upper(),end.upper()))
(root/'data/lcc-numeric-relative-spans.tsv').write_text(''.join('\t'.join(r)+'\n' for r in sorted(rows)))
print(f'{len(rows)} documented numeric-relative Cutter spans')
