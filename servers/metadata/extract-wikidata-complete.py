#!/usr/bin/env python3
"""Resume the unprocessed tail first, then backfill the prefix using one projection.

Original v1 files are retained. Consumers use this bundle only after completion.
"""
import argparse,fcntl,importlib.util,json,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('extract_wikidata',Path(__file__).with_name('extract-wikidata.py'))
extractor=importlib.util.module_from_spec(spec);spec.loader.exec_module(extractor)


def run(source,index_path,output,boundary,chunk_records=200000,threads=2,min_free_bytes=100*1024**3):
    source,index_path,output=map(Path,(source,index_path,output));output.mkdir(parents=True,exist_ok=True)
    with (output/'.lock').open('w') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        index=extractor.load_index(source,index_path);total=index['records']
        if not 0<=boundary<=total or (boundary!=total and str(boundary) not in index['record_offsets']):
            raise ValueError('backfill boundary is not an indexed checkpoint')
        parts=[]
        if boundary:parts.append(dict(directory='prefix',start=0,stop=boundary))
        if boundary<total:parts.append(dict(directory='tail',start=boundary,stop=total))
        manifest=dict(version=1,projection_version=extractor.VERSION,source=index['source'],total_records=total,
                      boundary=boundary,chunk_records=chunk_records,parts=parts)
        path=output/'manifest.json'
        if path.exists() and json.loads(path.read_text())!=manifest:raise ValueError('bundle configuration changed')
        extractor.atomic_json(path,manifest)
        def report(status,phase):
            done=0;counts={}
            for part in parts:
                p=output/part['directory']/'progress.json'
                if p.exists():
                    d=json.loads(p.read_text());done+=d['counts']['records']
                    for key,value in d['counts'].items():counts[key]=counts.get(key,0)+value
            d=dict(status=status,phase=phase,records=done,total_records=total,counts=counts,updated_at_unix=int(time.time()))
            extractor.atomic_json(output/'progress.json',d);print(json.dumps(d),flush=True)
        try:
            for part in sorted(parts,key=lambda p:p['directory']!='tail'):
                phase='extracting_tail' if part['directory']=='tail' else 'backfilling_prefix'
                report('running',phase)
                extractor.extract(source,index_path,output/part['directory'],start=part['start'],stop=part['stop'],
                                  chunk_records=chunk_records,threads=threads,min_free_bytes=min_free_bytes)
                report('running',phase)
            report('complete','complete')
        except BaseException:
            report('failed','retry_required');raise


def completed_chunks(output):
    """Yield original record order only after every v2 range has completed."""
    output=Path(output);m=json.loads((output/'manifest.json').read_text())
    if json.loads((output/'progress.json').read_text())['status']!='complete':
        raise ValueError('classifier backfill is not complete')
    cursor=0
    for part in m['parts']:
        if part['start']!=cursor:raise ValueError('noncontiguous bundle')
        directory=output/part['directory'];pm=json.loads((directory/'manifest.json').read_text())
        progress=json.loads((directory/'progress.json').read_text())
        if pm['version']!=m['projection_version'] or pm['source']!=m['source'] or progress['status']!='complete':
            raise ValueError('incomplete or incompatible part')
        for marker in sorted(directory.glob('*.meta.json')):
            meta=json.loads(marker.read_text());path=directory/meta['file']
            if meta['start']!=cursor or meta['version']!=m['projection_version'] or meta['end']>part['stop']:
                raise ValueError('invalid chunk range or version')
            if path.stat().st_size!=meta['compressed_bytes'] or extractor.file_hash(path)!=meta['sha256']:
                raise ValueError('corrupt chunk')
            cursor=meta['end'];yield path
        if cursor!=part['stop']:raise ValueError('incomplete part')
    if cursor!=m['total_records']:raise ValueError('incomplete bundle')


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('source');p.add_argument('index');p.add_argument('output');p.add_argument('--boundary',type=int,required=True)
    p.add_argument('--chunk-records',type=int,default=200000);p.add_argument('--threads',type=int,default=2)
    a=p.parse_args();run(a.source,a.index,a.output,a.boundary,a.chunk_records,a.threads)
