#!/usr/bin/env python3
"""Verify linked MARC evidence and export exact reviewed preliminary intervals.

Trailing initials remain unresolved. The output retains the full interval
explicitly present in the source; it does not generate table subdivisions.
"""
import argparse, hashlib, json, re
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--data',type=Path);p.add_argument('--output',type=Path);a=p.parse_args()
data=a.data or Path(__file__).resolve().parents[3] / 'shared/subject-projection'/'data';output=a.output or data/'lcc-reviewed-preliminary-spans.tsv'
rows={}
for r in json.loads((data/'lcc-reviewed-preliminary-spans.json').read_text())['ranges']:
 edition_bytes=(data/r['edition_path']).read_bytes();marc=(data/r['marc_path']).read_bytes()
 assert hashlib.sha256(edition_bytes).hexdigest()==r['edition_sha256']
 assert hashlib.sha256(marc).hexdigest()==r['marc_sha256']
 edition=json.loads(edition_bytes)
 assert edition['key']==r['edition_key'] and r['lccn'] in edition['lccn']
 assert r['notation'] in edition['lc_classifications']
 linked='marc:'+r['marc_url'].split('/download/',1)[1]+':'+str(r['marc_byte_start'])+':'+str(r['marc_byte_length'])
 assert linked in edition['source_records']
 assert len(marc)==r['marc_byte_length']==int(marc[:5]) and marc[-1:]==b'\x1d'
 base=int(marc[12:17]);directory=marc[24:base-1];assert len(directory)%12==0
 fields=[]
 for i in range(0,len(directory),12):
  e=directory[i:i+12];n=int(e[3:7]);at=int(e[7:12]);fields.append((e[:3].decode(),marc[base+at:base+at+n-1].decode()))
 assert next(v.strip() for t,v in fields if t=='001')==r['lccn']
 # MARC permits repeated $a fields. Do not discard the second classification.
 calls=[sub[1:] for tag,value in fields if tag=='050' for sub in value[2:].split('\x1f')[1:] if sub.startswith('a')]
 assert r['notation'] in calls, 'Reviewed form must appear verbatim in MARC 050$a'
 m=re.fullmatch(r'([A-Z]+\d+(?:\.\d+)?)(\.[A-Z]\d*)-(Z\d*)\.?([A-Y])',r['notation'])
 assert m and not m[1].startswith('K') and m[2][1]<=m[4]
 derived=m[1]+m[2]+'-'+m[1]+'.'+m[3]
 assert r['span']==derived and r['notation'] not in rows
 rows[r['notation']]=derived
output.write_text(''.join(k+'\t'+v+'\n' for k,v in sorted(rows.items())))
print(f'{len(rows)} exact preliminary intervals; linked MARC 050 fields, identities and hashes verified')
