import importlib.util,sqlite3,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('bibliography',Path(__file__).with_name('extend-wikidata-bibliography.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
def claim(v,rank='normal'):return {'rank':rank,'mainsnak':{'snaktype':'value','datavalue':{'value':v}}}
class BibliographyTest(unittest.TestCase):
 def test_fields_ranks_and_non_book_exclusion(self):
  c=sqlite3.connect(':memory:');c.executescript(m.m.SCHEMA+m.SCHEMA)
  claims={'P31':[claim({'id':'Q571'})],'P1476':[claim({'language':'en','text':'Real title'})],'P1680':[claim({'language':'en','text':'Subtitle'})],'P577':[claim({'time':'+2001-01-01T00:00:00Z','precision':9})],'P123':[claim({'id':'Q2'}),claim({'id':'Q3'},'deprecated')],'P50':[claim({'id':'Q1'})]}
  m.insert(c,{'id':'Q10','labels':{'en':{'value':'Label'}},'claims':claims})
  self.assertEqual(c.execute('SELECT * FROM bibliography').fetchall(),[('Q10','Real title','Subtitle',2001)])
  self.assertEqual(c.execute('SELECT publisher FROM book_publisher').fetchall(),[('Q2',)])
  m.insert(c,{'id':'Q11','labels':{'en':{'value':'Paper'}},'claims':{'P31':[claim({'id':'Q13442814'})],'P50':[claim({'id':'Q1'})]}})
  self.assertEqual(c.execute('SELECT count(*) FROM bibliography').fetchone()[0],1)
  self.assertIsNone(m.title_value({'P1680':[claim({'language':'en','text':'First'}),claim({'language':'en','text':'Second'})]},'P1680'))
if __name__=='__main__':unittest.main()

class BackfillTest(unittest.TestCase):
 def test_publisher_second_pass_and_resume(self):
  import tempfile,json,subprocess,hashlib
  from unittest.mock import patch
  with tempfile.TemporaryDirectory() as temp:
   root=Path(temp);db=root/'input.sqlite';c=sqlite3.connect(db);c.executescript(m.m.SCHEMA);c.execute("INSERT INTO state VALUES('status','complete')");c.commit();c.close()
   records=[{'id':'Q2','labels':{'en':{'value':'Publisher name'}}}, {'id':'Q10','labels':{'en':{'value':'Book'}},'claims':{'P31':[claim({'id':'Q571'})],'P123':[claim({'id':'Q2'})]}}]
   chunk=root/'chunk.jsonl.zst';chunk.write_bytes(subprocess.check_output(['zstd','-q','-c'],input=b''.join(json.dumps(r).encode()+b'\n' for r in records)));chunk.with_suffix('.meta.json').write_text(json.dumps({'sha256':hashlib.sha256(chunk.read_bytes()).hexdigest()}))
   with patch.object(m.m.bundle,'completed_chunks',return_value=iter([chunk])):m.extend(root,db)
   m.extend(root,db)
   c=sqlite3.connect(db);self.assertEqual(c.execute('SELECT name FROM book_publisher').fetchone()[0],'Publisher name');self.assertEqual(c.execute('SELECT count(*) FROM bibliography_chunk').fetchone()[0],2)
