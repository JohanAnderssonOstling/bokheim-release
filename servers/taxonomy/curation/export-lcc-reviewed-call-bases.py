#!/usr/bin/env python3
"""Export exact reviewed call bases, verifying preserved edition and MARC evidence."""
from pathlib import Path
import argparse,json,hashlib,re,csv
p=argparse.ArgumentParser();p.add_argument('--root',type=Path);p.add_argument('--data',type=Path);p.add_argument('--output',type=Path);a=p.parse_args()
root=a.root or Path(__file__).resolve().parents[3];data=a.data or root/'shared/subject-projection/data';output=a.output or data/'lcc-reviewed-call-bases.tsv'
known=set()
for r in csv.DictReader((root/'shared/subject-projection/data/lcc-2024-outline.csv').open()):known.update([r['start_letters'],r['end_letters']])
norm=lambda s:' '.join(s.split()).upper();rows={}
for r in json.loads((data/'lcc-reviewed-call-bases.json').read_text())['corrections']:
 raw=(data/r['edition_path']).read_bytes();marc=(data/r['marc_path']).read_bytes()
 assert hashlib.sha256(raw).hexdigest()==r['edition_sha256'] and hashlib.sha256(marc).hexdigest()==r['marc_sha256'],'Evidence changed'
 edition=json.loads(raw);assert edition['key']==r['edition_key'];assert r['lccn'] in edition['lccn'] and r['isbn'] in edition['isbn_13']
 assert norm(r['notation'])==r['notation'] and r['notation'] in [norm(x) for x in edition['lc_classifications']]
 assert r['verified_call'] in edition['lc_classifications']
 linked='marc:'+r['marc_url'].split('/download/',1)[1]+':'+str(r['marc_byte_start'])+':'+str(r['marc_byte_length'])
 assert linked in edition['source_records'],'MARC record is not linked by the edition'
 assert r['notation'].isascii()
 assert len(marc)==r['marc_byte_length']==int(marc[:5]) and marc.endswith(b'\x1d');base=int(marc[12:17]);directory=marc[24:base-1];assert len(directory)%12==0
 fields=[]
 for i in range(0,len(directory),12):
  ent=directory[i:i+12];tag=ent[:3].decode();n=int(ent[3:7]);at=int(ent[7:12]);fields.append((tag,marc[base+at:base+at+n-1].decode('utf8')))
 assert next(v.strip() for t,v in fields if t=='001')==r['lccn']
 calls=[];isbns=[]
 for tag,value in fields:
  sub={s[0]:s[1:] for s in value[2:].split('\x1f')[1:]}
  if tag=='050' and 'a' in sub and 'b' in sub:calls.append(sub['a']+' '+sub['b'])
  if tag=='020' and 'a' in sub:isbns.append(sub['a'].split()[0])
 assert r['verified_call'] in calls and r['isbn'] in isbns
 stem,year=r['verified_call'].rsplit(' ',1);assert re.fullmatch(r'\d{4}[a-z]?',year)
 m=re.fullmatch(r'([A-Z]+)(\d+(?:\.\d+)?)(.*)',stem);assert m and m[1] in known and not m[1].startswith('K')
 tail=re.sub(r'[\s.]','',m[3]);cutters=re.findall(r'[A-Z]\d+',tail);assert cutters and ''.join(cutters)==tail
 derived=m[1]+m[2]+''.join('.'+x for x in cutters);assert r['base']==derived,'Base does not match the verified call'
 assert r['notation'] not in rows;rows[r['notation']]=derived
output.write_text(''.join(k+'\t'+v+'\n' for k,v in sorted(rows.items())))
print(f'{len(rows)} reviewed call bases; edition/MARC identities and hashes verified')
