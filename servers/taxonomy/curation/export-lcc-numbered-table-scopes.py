#!/usr/bin/env python3
"""Expand exact, unmodified P-PZ numeric table bindings without local overrides."""
import argparse,collections,csv,decimal,json,pathlib,re,sqlite3
p=argparse.ArgumentParser();p.add_argument('--root',type=pathlib.Path);p.add_argument('--output',type=pathlib.Path);p.add_argument('--report',type=pathlib.Path);args=p.parse_args()
root=args.root or pathlib.Path(__file__).resolve().parents[3]
data=root/'shared/subject-projection/data';output=args.output or data/'lcc-numbered-table-scopes.tsv'
c=sqlite3.connect((data/'lcc-mdsconnect-2016.sqlite3').resolve().as_uri()+'?mode=ro',uri=True);c.row_factory=sqlite3.Row
known=set()
for r in csv.DictReader((data/'lcc-2024-outline.csv').open()):known.update([r['start_letters'],r['end_letters']])
D=decimal.Decimal;table_roots=collections.defaultdict(list)
for r in c.execute("SELECT s.*,r.caption FROM classification_span s JOIN classification_record r USING(record_id) WHERE table_id LIKE 'P-PZ%' AND caption LIKE 'Table for %'"):
    if r['start_number'] in ('0','1') and re.fullmatch(r'\d+',r['end_number'] or ''):table_roots[r['table_id']].append(dict(r))
entries=[dict(r) for r in c.execute('SELECT * FROM reference_entry WHERE table_id IS NULL')]
by_class=collections.defaultdict(list)
for r in entries:
    m=re.match(r'^([A-Z]+)(\d+(?:\.\d+)?)',r['start_number'] or '')
    if m:by_class[m[1]].append((D(m[2]),r))
pages={};bindings=[];mismatched=[];local_review=[]
for r in entries:
    a=re.fullmatch(r'([A-Z]+)(\d+)',r['start_number'] or '');z=re.fullmatch(r'([A-Z]+)(\d+)',r['end_number'] or '')
    if not a or not z or a[1]!=z[1] or a[1] not in known or a[1].startswith('K'):continue
    key=(r['document_id'],r['page'])
    if key not in pages:pages[key]=c.execute('SELECT text FROM reference_page WHERE document_id=? AND page=?',key).fetchone()[0]
    for line in pages[key].splitlines():
        if not re.match(r'^\s*'+re.escape(r['notation'])+r'\s',line):continue
        # Modified bindings and wrapped/ambiguous citations need separate review.
        m=re.search(r'\(Table (P-PZ\d+)\)$',line.strip())
        if not m:continue
        width=int(z[2])-int(a[2])+1;table=m[1]
        roots=[x for x in table_roots[table] if int(x['end_number'])-int(x['start_number'])+1==width]
        b=dict(entry_id=r['entry_id'],start=r['start_number'],end=r['end_number'],table=table,document_id=r['document_id'],page=r['page'],source_line=line.strip())
        if len(roots)!=1:mismatched.append(b);continue
        # Local printed subdivisions may override even a table not labeled modified.
        local=[x['entry_id'] for n,x in by_class[a[1]] if D(a[2])<=n<=D(z[2]) and (x['start_number'],x['end_number'])!=(r['start_number'],r['end_number'])]
        if local:local_review.append(dict(b,local_entries=local));continue
        b.update(letters=a[1],offset=int(a[2])-int(roots[0]['start_number']),table_start=roots[0]['start_number'],table_end=roots[0]['end_number'])
        bindings.append(b)
def endpoint(value,b):
    m=re.fullmatch(r'(\d+(?:\.\d+)?)(.*)',value or '')
    if not m or not re.fullmatch(r'(?:\.?[A-Z][0-9]+)*(?:\.?[A-Z])?',m[2]):return None
    n=D(m[1])
    if not D(b['table_start'])<=n<=D(b['table_end']):return None
    number=format(D(b['offset'])+n,'f')
    if '.' in number:number=number.rstrip('0').rstrip('.')
    return b['letters']+number+''.join('.'+x for x in re.findall(r'[A-Z][0-9]*',m[2]))
scopes=collections.defaultdict(set);blocked=set()
for b in bindings:
    for r in c.execute('SELECT start_number,end_number FROM classification_span WHERE table_id=?',(b['table'],)):
        a=endpoint(r['start_number'],b)
        if a is None:continue
        z=endpoint(r['end_number'] or r['start_number'],b)
        if z is None:blocked.add(a)
        else:scopes[a].add(z)
rows=sorted(a+'\t'+z for a,ends in scopes.items() if a not in blocked for z in ends)
output.write_text('\n'.join(rows)+'\n')
if args.report:args.report.write_text(json.dumps(dict(bindings=bindings,mismatched_bindings=mismatched,local_review=local_review,blocked_starts=sorted(blocked),scopes=len(rows)),indent=2)+'\n')
print(f'{len(rows)} scopes from {len(bindings)} bindings; {len(local_review)} local exceptions and {len(mismatched)} mismatched bindings deferred; {len(blocked)} blocked starts')
