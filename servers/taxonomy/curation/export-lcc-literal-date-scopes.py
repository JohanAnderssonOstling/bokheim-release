from pathlib import Path
import sqlite3,re,json,argparse,decimal
parser=argparse.ArgumentParser(description='Export documented literal item-date positions; no date inference.')
parser.add_argument('--root',type=Path);parser.add_argument('--output',type=Path);parser.add_argument('--report',type=Path);args=parser.parse_args()
root=args.root or Path(__file__).resolve().parents[3]
output=args.output or root/'shared/subject-projection/data/lcc-literal-date-scopes.tsv'
c=sqlite3.connect((root/'shared/subject-projection/data/lcc-mdsconnect-2016.sqlite3').as_uri()+'?mode=ro',uri=True);c.row_factory=sqlite3.Row
scopes={};pages={};deferred=[]
assert c.execute("select count(*) from classification_span s join reference_current_record r using(record_id) where s.table_id='BS2' and s.start_number='.x date' and s.end_number is null and r.caption='Texts. By date'").fetchone()[0]==1
bs_entries=list(c.execute("select entry_id,start_number,end_number from reference_entry where table_id is null and start_number like 'BS%'"))
def bs_key(value):
 m=re.fullmatch(r'BS(\d+(?:\.\d+)?)\.([A-Z])(\d+)',value or '')
 return (decimal.Decimal(m[1]),m[2],decimal.Decimal('0.'+m[3])) if m else None
def canonical(value):
 m=re.fullmatch(r'([A-Z]+\d+(?:\.\d+)?)(.*)',value)
 if not m or m[1].startswith('K'):return None
 tail=m[2].replace('.','');parts=re.findall(r'[A-Z]\d+',tail)
 if ''.join(parts)!=tail:return None
 return m[1]+''.join('.'+x for x in parts)
for r in c.execute("select s.*,r.caption,r.control_number from classification_span s join reference_current_record r using(record_id) where s.table_id is null and s.start_number like '% date' and s.end_number is null"):
 base=canonical(r['start_number'][:-5])
 if base:scopes[base]=dict(source='explicit date template',record_id=r['record_id'],notation=r['start_number'],caption=r['caption'],control_number=r['control_number'])
for r in c.execute("select * from reference_entry where table_id is null and start_number like 'BS%' and end_number is not null"):
 key=r['document_id'],r['page']
 if key not in pages:pages[key]=c.execute('select text from reference_page where document_id=? and page=?',key).fetchone()[0]
 for line in pages[key].splitlines():
  if not re.match(r'^\s*'+re.escape(r['notation'])+r'\s',line) or not line.strip().endswith('(Table BS2)'):continue
  a,z=canonical(r['start_number']),canonical(r['end_number'])
  if not a or z!=a+'2':deferred.append(dict(start=r['start_number'],end=r['end_number'],line=line));continue
  lo,hi=bs_key(a),bs_key(z)
  if lo is None or hi is None:continue
  local=[e['entry_id'] for e in bs_entries if (k:=bs_key(e['start_number'])) is not None and lo<=k<=hi and (e['start_number'],e['end_number'])!=(r['start_number'],r['end_number'])]
  if local:deferred.append(dict(start=a,end=z,local_entries=local));continue
  scopes[a]=dict(source='unmodified BS2 binding',entry_id=r['entry_id'],document_id=r['document_id'],page=r['page'],line=line.strip(),start=r['start_number'],end=r['end_number'])
output.write_text('\n'.join(sorted(scopes))+'\n')
if args.report:args.report.write_text(json.dumps({'scopes':scopes,'deferred':deferred},indent=2)+'\n')
print(f'{len(scopes)} documented literal-date positions; {len(deferred)} bindings deferred')
