import bz2
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('extract_wikidata', Path(__file__).with_name('extract-wikidata.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def claim(value, datatype='string', rank='normal'):
    return dict(id='Q1$statement', rank=rank, mainsnak=dict(datatype=datatype, snaktype='value', datavalue=dict(value=value)),
                qualifiers={'P1545':[{'datavalue':{'value':'2'}}]}, references=[{'snaks':{'P248':[{'datavalue':{'value':{'id':'Q2'}}}]}}])


class ExtractionTests(unittest.TestCase):
    def fixture(self, root, records):
        raw = b'[\n'
        offsets={'0':0}
        for i,record in enumerate(records,1):
            raw += json.dumps(record).encode() + (b',\n' if i < len(records) else b'\n')
            offsets[str(i)] = len(raw)
        raw += b']\n'
        offsets[str(len(records))]=len(raw)
        source = root/'dump.bz2'
        source.write_bytes(bz2.compress(raw))
        with module.indexed_bzip2.open(str(source),parallelization=1) as f:
            f.read()
            blocks=list(f.block_offsets().items())
        index=root/'index.json'
        index.write_text(json.dumps(dict(version=1,source=module.fingerprint(source),records=len(records),
                                        decoded_size=len(raw),record_offsets=offsets,checkpoint_interval=1,block_offsets=blocks)))
        return source,index

    def records(self):
        return [{'id':f'Q{i}','type':'item','labels':{'sv':{'value':f'Namn {i}'}},'claims':{'P50':[claim({'id':'Q100'},'wikibase-item')]}} for i in range(1,6)]

    def test_projection_preserves_all_ids_full_claims_and_languages(self):
        value=claim('0000123', 'external-id', 'deprecated')
        book={'id':'Q1','labels':{'en':{'value':'Title'},'sv':{'value':'Titel'}},
              'descriptions':{'sv':{'value':'Beskrivning'}},'claims':{
                  'P999999':[value],'P1036':[claim('100','external-id')], 'P8359':[claim('100')],
                  'P8360':[claim('QA76')], 'P655':[claim({'id':'Q2'},'wikibase-item')],
                  'P101':[claim({'id':'Q3'},'wikibase-item')], 'P999998':[claim('unrequested text')]},
              'sitelinks':{'svwiki':{'title':'Titel'},'commonswiki':{'title':'Category:Title'}}}
        result=module.project(book)
        self.assertEqual(result['claims']['P999999'],[value])
        self.assertEqual(result['claims']['P8360'],book['claims']['P8360'])
        self.assertEqual(result['claims']['P655'],book['claims']['P655'])
        self.assertEqual(result['claims']['P1036'],book['claims']['P1036'])
        self.assertEqual(result['claims']['P8359'],book['claims']['P8359'])
        self.assertEqual(result['claims']['P999998'],book['claims']['P999998'])
        self.assertEqual(result['labels'],book['labels'])
        self.assertEqual(result['descriptions'],book['descriptions'])
        self.assertEqual(result['sitelinks'],{'svwiki':{'title':'Titel'}})

    def test_resume_has_no_duplicate_records_and_retains_unresolved_author_links(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source,index=self.fixture(root,self.records());out=root/'out'
            module.extract(source,index,out,chunk_records=2,threads=1,max_chunks=1,min_free_bytes=0)
            self.assertEqual(json.loads((out/'progress.json').read_text())['status'],'paused')
            first=next(out.glob('*.zst'));mtime=first.stat().st_mtime_ns
            module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)
            self.assertEqual(first.stat().st_mtime_ns,mtime)
            records=[]
            for path in sorted(out.glob('*.zst')):
                data=subprocess.check_output(['zstd','-q','-dc',str(path)])
                records.extend(json.loads(line) for line in data.splitlines())
            self.assertEqual([r['id'] for r in records],['Q1','Q2','Q3','Q4','Q5'])
            self.assertEqual(records[0]['claims']['P50'][0]['mainsnak']['datavalue']['value'],{'id':'Q100'})
            progress=json.loads((out/'progress.json').read_text())
            self.assertTrue(progress['full_dump']);self.assertEqual(progress['status'],'complete')
            self.assertEqual(progress['counts']['records'],5)
            first.write_bytes(b'corrupted')
            with self.assertRaises(ValueError):
                module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)

    def test_sample_is_marked_partial_and_manifest_cannot_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source,index=self.fixture(root,self.records());out=root/'out'
            module.extract(source,index,out,start=2,stop=4,chunk_records=2,threads=1,min_free_bytes=0)
            self.assertFalse(json.loads((out/'progress.json').read_text())['full_dump'])
            with self.assertRaises(ValueError):
                module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)
            source.write_bytes(source.read_bytes()+b'changed')
            with self.assertRaises(ValueError):
                module.extract(source,index,root/'other',chunk_records=2,threads=1,min_free_bytes=0)

    def test_interrupted_chunk_is_not_committed_and_can_be_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source,index=self.fixture(root,self.records());out=root/'out'
            project=module.project
            def fail(record):
                if record['id']=='Q4':raise RuntimeError('interrupted')
                return project(record)
            with patch.object(module,'project',side_effect=fail):
                with self.assertRaises(RuntimeError):
                    module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)
            self.assertEqual(len(list(out.glob('*.meta.json'))),1)
            module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)
            self.assertEqual(json.loads((out/'progress.json').read_text())['counts']['records'],5)

    def test_wrong_checkpoint_cannot_be_published(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source,index=self.fixture(root,self.records());out=root/'out'
            value=json.loads(index.read_text());value['record_offsets']['2']+=1;index.write_text(json.dumps(value))
            with self.assertRaises(ValueError):
                module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=0)
            self.assertFalse(list(out.glob('*.meta.json')))

    def test_disk_reserve_stops_before_writing_chunk(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source,index=self.fixture(root,self.records());out=root/'out'
            with self.assertRaises(RuntimeError):
                module.extract(source,index,out,chunk_records=2,threads=1,min_free_bytes=10**30)
            self.assertFalse(list(out.glob('*.zst')))


if __name__=='__main__':unittest.main()
