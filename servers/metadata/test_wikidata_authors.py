import importlib.util,sqlite3,tempfile,json,gzip,hashlib,subprocess,unittest
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('author_index',Path(__file__).with_name('index-wikidata-authors.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
def claim(value,rank='normal',precision=None):
    return {'rank':rank,'mainsnak':{'snaktype':'value','datavalue':{'value':value}}}
class AuthorIndexTest(unittest.TestCase):
    def test_deprecated_claims_and_isbn_checksums(self):
        c=sqlite3.connect(':memory:');c.executescript(m.SCHEMA)
        m.insert(c,{'id':'Q10','claims':{'P212':[claim('9780306406157'),claim('9780306406158')],'P50':[claim({'id':'Q1'},'deprecated'),claim({'id':'Q2'})]}})
        self.assertEqual(c.execute('SELECT * FROM isbn').fetchall(),[('9780306406157','Q10')])
        self.assertEqual(c.execute('SELECT author FROM credit').fetchall(),[('Q2',)])
        self.assertEqual(m.values({'P50':[claim({'id':'Q1'},'deprecated')]},'P50'),[])
    def test_build_inherits_work_authors_and_is_restartable(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);chunk=root/'chunk.jsonl.zst';authors=root/'authors.gz';out=root/'authors.sqlite'
            records=[{'id':'Q10','labels':{'en':{'value':'Edition'}},'claims':{'P957':[claim('0-306-40615-2')],'P629':[claim({'id':'Q100'})]}},
                     {'id':'Q100','claims':{'P50':[claim({'id':'Q1'})]}},
                     {'id':'Q1','labels':{'en':{'value':'Alex Smith'}},'claims':{'P31':[claim({'id':'Q5'})],'P214':[claim('123')],'P569':[claim({'time':'+1900-01-01T00:00:00Z','precision':9})]}}]
            raw=b''.join(json.dumps(r).encode()+b'\n' for r in records)
            chunk.write_bytes(subprocess.check_output(['zstd','-q','-c'],input=raw))
            chunk.with_suffix('.meta.json').write_text(json.dumps({'sha256':hashlib.sha256(chunk.read_bytes()).hexdigest()}))
            (root/'manifest.json').write_text('{}')
            with gzip.open(authors,'wb') as f:f.write(json.dumps({'key':'/authors/OL1A','birth_date':'1900','alternate_names':['Smith, Alex']}).encode()+b'\n')
            # Stop after a committed extraction chunk; resume must not duplicate.
            with patch.object(m.bundle,'completed_chunks',return_value=iter([chunk])),patch.object(m.gzip,'open',side_effect=RuntimeError('interrupted')):
                with self.assertRaisesRegex(RuntimeError,'interrupted'):m.build(root,authors,out,'2026-07-31')
            with patch.object(m.bundle,'completed_chunks',return_value=iter([chunk])):m.build(root,authors,out,'2026-07-31')
            c=sqlite3.connect(out)
            self.assertEqual(c.execute('SELECT isbn13,book,work,author,source FROM isbn_author').fetchall(),[('9780306406157','Q10','Q100','Q1','Q100')])
            self.assertEqual(c.execute('SELECT birth_year FROM ol_profile').fetchone()[0],1900)
            self.assertEqual(c.execute('SELECT birth_year FROM profile WHERE qid="Q1"').fetchone()[0],1900)
            self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0],'ok')
            self.assertFalse(Path(str(out)+'.building').exists())
    def test_uncertain_dates_are_not_identity_conflicts(self):
        self.assertIsNone(m.ol_year('circa 1900'));self.assertIsNone(m.ol_year('1900?'))
        self.assertIsNone(m.year({'P569':[claim({'time':'+1900-01-01T00:00:00Z','precision':8})]},'P569'))
if __name__=='__main__':unittest.main()
