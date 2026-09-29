#!/usr/bin/env python3
"""Restartable extraction/backfill followed by the staging classification index."""
import importlib.util,argparse
from pathlib import Path

def load(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('source');p.add_argument('index');p.add_argument('output');p.add_argument('--boundary',type=int,required=True)
    a=p.parse_args()
    bundle=load('complete','extract-wikidata-complete.py');bundle.run(a.source,a.index,a.output,a.boundary)
    index=load('classifier_index','index-wikidata-classifiers.py');index.build(a.output,str(Path(a.output)/'classifiers.sqlite'))
