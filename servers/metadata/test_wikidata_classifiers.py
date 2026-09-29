import importlib.util,json,subprocess,tempfile,unittest,sqlite3
from pathlib import Path
from unittest.mock import patch
from test_extract_wikidata import ExtractionTests,claim

def load(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
classifiers=load('classifiers','wikidata-classifiers.py')
bundle=load('bundle','extract-wikidata-complete.py')

class ClassifierTests(unittest.TestCase):
    def test_inference_is_separate_and_preserves_full_link_and_source(self):
        link=claim({'id':'Q2'},'wikibase-item');ddc=claim('700','external-id','deprecated')
        book={'id':'Q1','claims':{'P8360':[claim('N5300')],'P921':[link]}}
        topic={'id':'Q2','claims':{'P1036':[ddc],'P8359':[claim('999')]}}
        r=classifiers.for_book(book,{'Q2':topic})
        self.assertEqual(len(r['direct']),1);self.assertFalse(r['direct'][0]['inferred'])
        self.assertEqual(len(r['inferred']),1)
        e=r['inferred'][0];self.assertTrue(e['inferred']);self.assertEqual(e['source_entity'],'Q2')
        self.assertEqual(e['statement'],ddc);self.assertEqual(e['topic_link'],link)
        self.assertEqual(classifiers.for_book(book,{})['inferred'],[])
    def test_preferred_then_normal_then_deprecated_fallback(self):
        old=claim('100',rank='deprecated');normal=claim('200');preferred=claim('300',rank='preferred')
        self.assertEqual(classifiers.values([old,normal,preferred]),[preferred])
        self.assertEqual(classifiers.values([old,normal]),[normal])
        self.assertEqual(classifiers.values([old]),[old])
    def test_new_classifier_definition_and_nonidentifier_values_survive(self):
        definition={'id':'P999998','type':'property','claims':{'P31':[claim({'id':classifiers.TOPIC_CLASS},'wikibase-item')]}}
        r={'id':'Q1','claims':{'P999998':[claim('new code')]}}
        projected=bundle.extractor.project(r)
        self.assertEqual(classifiers.assigned(projected,classifiers.definitions([definition]))[0]['value'],'new code')
    def test_all_known_classifiers_preserve_complete_statements(self):
        r={'id':'Q1','claims':{p:[claim('test',rank='deprecated')] for p in classifiers.KNOWN}}
        self.assertEqual(bundle.extractor.project(r)['claims'],r['claims'])
    def test_index_views_keep_topic_inference_separate(self):
        index=load('classifier_index','index-wikidata-classifiers.py')
        c=sqlite3.connect(':memory:');c.executescript(index.SCHEMA)
        book={'id':'Q1','claims':{'P212':[claim('9780306406157')], 'P8360':[claim('N5300')], 'P921':[claim({'id':'Q2'},'wikibase-item')]}}
        topic={'id':'Q2','claims':{'P1036':[claim('700')], 'P8359':[claim('999')]}}
        index.insert(c,book,classifiers.KNOWN);index.insert(c,topic,classifiers.KNOWN)
        self.assertEqual(c.execute('SELECT value,inferred FROM isbn_classification_direct').fetchall(),[('N5300',0)])
        self.assertEqual(c.execute('SELECT value,inferred,source_topic FROM isbn_classification_inferred').fetchall(),[('700',1,'Q2')])
        self.assertEqual(index.canonical_isbn('0306406152'),'9780306406157')
        self.assertIsNone(index.canonical_isbn('9780306406158'))

    def test_tail_then_backfill_and_resumption_produce_one_complete_sequence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);fixture=ExtractionTests();records=fixture.records()
            records[0]['claims']['P8359']=[claim('100')]
            records[0]['claims']['P212']=[claim('9780306406157')]
            source,index=fixture.fixture(root,records);out=root/'bundle'
            original=bundle.extractor.extract;calls=[]
            def interrupt(*a,**kw):
                calls.append(kw['start'])
                if kw['start']==0:raise RuntimeError('backfill interrupted')
                return original(*a,**kw)
            with patch.object(bundle.extractor,'extract',side_effect=interrupt):
                with self.assertRaises(RuntimeError):bundle.run(source,index,out,2,2,1,0)
            self.assertEqual(calls,[2,0])
            with self.assertRaises(ValueError):list(bundle.completed_chunks(out))
            bundle.run(source,index,out,2,2,1,0)
            found=[]
            for path in bundle.completed_chunks(out):
                found.extend(json.loads(x) for x in subprocess.check_output(['zstd','-q','-dc',str(path)]).splitlines())
            self.assertEqual([r['id'] for r in found],['Q1','Q2','Q3','Q4','Q5'])
            self.assertIn('P8359',found[0]['claims'])
            self.assertEqual(json.loads((out/'progress.json').read_text())['records'],5)
            index=load('classifier_index','index-wikidata-classifiers.py')
            database=out/'classifiers.sqlite';index.build(out,database);index.build(out,database)
            c=sqlite3.connect(database)
            self.assertEqual(c.execute('SELECT isbn13,value FROM isbn_classification_direct').fetchall(),[('9780306406157','100')])
            self.assertEqual(c.execute('SELECT count(*) FROM completed_chunk').fetchone()[0],3)
            c.close()

if __name__=='__main__':unittest.main()
