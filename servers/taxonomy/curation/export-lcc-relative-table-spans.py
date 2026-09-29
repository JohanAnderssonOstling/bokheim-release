#!/usr/bin/env python3
"""Export documented relative Cutter spans from the checked-in LC reference."""
import pathlib,re,sqlite3
root=pathlib.Path(__file__).resolve().parents[3] / 'shared/subject-projection'
ids=set((root/'data/lcc-table-identifiers.txt').read_text().splitlines())
pattern=re.compile(r"\.x[A-Z][0-9]*(?:[A-Z][0-9]+)*")
c=sqlite3.connect((root/'data/lcc-mdsconnect-2016.sqlite3').resolve().as_uri()+'?mode=ro',uri=True)
rows=set()
for table,start,end in c.execute('SELECT table_id,start_number,end_number FROM classification_span'):
    end=end or start
    if table and table.upper() in ids and start and end and pattern.fullmatch(start) and pattern.fullmatch(end):
        rows.add('\t'.join((table.upper(),start.upper(),end.upper())))
(root/'data/lcc-relative-table-spans.tsv').write_text('\n'.join(sorted(rows))+'\n')
print(f'{len(rows)} documented relative Cutter spans')
