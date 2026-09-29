#!/usr/bin/env python3
"""Export complete non-law reference scopes for explicit plus-marked starts."""
import pathlib,sqlite3,csv,re,collections,decimal,json,subprocess,sys
root=pathlib.Path(__file__).resolve().parents[3] / 'shared/subject-projection'
subprocess.run([sys.executable,str(pathlib.Path(__file__).resolve().parent/'export-lcc-numbered-table-scopes.py')],check=True)
known=set()
for row in csv.DictReader((root/'data/lcc-2024-outline.csv').open()):
    known.update([row['start_letters'],row['end_letters']])
def endpoint(value):
    m=re.fullmatch(r'([A-Z]+)([0-9]+(?:\.[0-9]+)?)(.*)',value or '')
    if not m or m[1].startswith('K') or m[1] not in known:return None
    if not re.fullmatch(r'(?:\.?[A-Z][0-9]+)*(?:\.?[A-Z])?',m[3]):return None
    tail=m[3].replace('.','');parts=re.findall(r'[A-Z][0-9]*',tail)
    if ''.join(parts)!=tail or any(len(part)==1 for part in parts[:-1]):return None
    number=format(decimal.Decimal(m[2]),'f')
    if '.' in number:number=number.rstrip('0').rstrip('.')
    return m[1]+number+''.join('.'+part for part in parts)
c=sqlite3.connect((root/'data/lcc-mdsconnect-2016.sqlite3').resolve().as_uri()+'?mode=ro',uri=True)
manifest=json.loads((root/'data/lcc-reference-scope-corrections.json').read_text())
corrections={r['record_id']:r for r in manifest['corrections']};applied=set()
for r in corrections.values():
    source=c.execute('SELECT url,sha256 FROM reference_document WHERE document_id=?',(r['document_id'],)).fetchone()
    assert source==(r.get('source_url',manifest['source_url']),r.get('source_sha256',manifest['source_sha256'])), 'Reviewed correction source changed'
    if 'source_line' in r:
        pages=[c.execute('SELECT text FROM reference_page WHERE document_id=? AND page=?',(r['document_id'],page)).fetchone() for page in r['pages']]
        assert all(pages), 'Reviewed correction source page missing'
        assert any(r['source_line'] in [line.strip() for line in page[0].splitlines()] for page in pages), 'Reviewed correction source line changed'
groups=collections.defaultdict(set);blocked=set()
for record_id,start,end in c.execute("SELECT record_id,start_number,end_number FROM classification_span WHERE table_id IS NULL OR table_id=''"):
    if record_id in corrections:
        correction=corrections[record_id]
        assert (start,end)==(correction['start_number'],correction['end_number']), 'Reviewed correction input changed'
        identity=c.execute('SELECT control_number,caption FROM classification_record WHERE record_id=?',(record_id,)).fetchone()
        assert identity==(correction['control_number'],correction['caption']), 'Reviewed correction record changed'
        end=correction['corrected_end_number'];applied.add(record_id)
    a=endpoint(start)
    if not a:continue
    b=endpoint(end or start)
    if not b:blocked.add(a)
    else:groups[a].add(b)
assert applied==set(corrections), 'Missing reviewed correction rows'
for line in (root/'data/lcc-numbered-table-scopes.tsv').read_text().splitlines():
    start,end=line.split('\t')
    a,b=endpoint(start),endpoint(end)
    assert a and b, 'Invalid generated numbered table scope'
    groups[a].add(b)
rows=sorted(a+'\t'+b for a,ends in groups.items() if a not in blocked for b in ends)
(root/'data/lcc-reference-scopes.tsv').write_text('\n'.join(rows)+'\n')
print(f'{len(rows)} scopes; {len(blocked)} starts excluded because a recorded endpoint is unsupported')
