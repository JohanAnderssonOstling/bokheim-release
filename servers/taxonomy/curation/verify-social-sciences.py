#!/usr/bin/env python3
"""Requirement-level verification of the reviewed Social Sciences migration."""
import csv
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import unittest

FOLDER=Path(sys.argv.pop(1))
LIVE='--live' in sys.argv
if LIVE:sys.argv.remove('--live')
spec=importlib.util.spec_from_file_location('migration',Path(__file__).with_name('improve-social-sciences.py'))
migration=importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)


class SocialSciencesReview(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.before=sqlite3.connect(f'file:{FOLDER}/before.sqlite3?mode=ro',uri=True)
        cls.after=sqlite3.connect(f'file:{FOLDER}/after.sqlite3?mode=ro',uri=True)
        cls.manifest=json.loads((FOLDER/'manifest.json').read_text())
        cls.new=cls.manifest['created']
        cls.merges={int(k):v for k,v in cls.manifest['merges'].items()}

    def canonical(self,id):
        while id in self.merges:id=self.merges[id]
        return id

    def parents(self,id):
        return {r[0] for r in self.after.execute('SELECT parent_concept_id FROM concept_parent WHERE concept_id=?',(id,))}

    def label(self,id):
        return self.after.execute('SELECT preferred_label FROM concept WHERE concept_id=?',(id,)).fetchone()[0]

    def test_integrity_and_reproducible_plan(self):
        migration.validate(self.after)
        copy=sqlite3.connect(':memory:')
        self.before.backup(copy)
        copy.execute('PRAGMA foreign_keys=ON')
        migration.improve(copy)
        self.assertEqual(migration.snapshot(copy),migration.snapshot(self.after))

    def test_no_new_sibling_collisions_or_orphans(self):
        q='SELECT p.parent_concept_id,c.preferred_label FROM concept c JOIN concept_parent p USING(concept_id) GROUP BY p.parent_concept_id,c.preferred_label HAVING count(*)>1'
        self.assertLessEqual(set(self.after.execute(q)),set(self.before.execute(q)))
        roots='SELECT concept_id FROM concept EXCEPT SELECT concept_id FROM concept_parent'
        self.assertEqual(set(self.after.execute(roots)),set(self.before.execute(roots)))

    def test_ddc_and_bisac_assignments(self):
        for table in ('ddc_selector','bisac_selector'):
            expected={(code,self.canonical(id)) for code,id in self.before.execute(f'SELECT selector,concept_id FROM {table}')}
            if table=='ddc_selector':
                for code,old,new in [('381',12269,5083),('304',12260,5488),('152',5389,5381),('655',5236,None)]:
                    expected.remove((code,old))
                    if new is not None:expected.add((code,new))
            self.assertEqual(expected,set(self.after.execute(f'SELECT selector,concept_id FROM {table}')))
        self.assertEqual(self.label(53287),'Unconscious Mind & Altered States')

    def test_broad_hd_vk_scopes(self):
        for lo,hi,id in [('HD28','HD9999',5104),('VK1','VK1661',478)]:
            self.assertEqual(self.after.execute('SELECT concept_id FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi)).fetchone(),(id,))
        self.assertEqual(self.label(478),'Navigation & Merchant Marine')
        for id in (5232,5234,5235,12272):self.assertIn(5104,self.parents(id))
        self.assertEqual(self.after.execute("SELECT concept_id FROM lcc_range WHERE start_code='VA10'").fetchone(),(13770,))

    def test_law_scopes(self):
        self.assertEqual(self.label(11731),'Private Law Unification')
        for lo,hi,id in [('K605','K615',11731),('K625','K709',5310),('K670','K709',5288),
                         ('K37','K44',self.new['Reference & Collections']),('K50','K54',self.new['Reference & Collections']),
                         ('K68','K70',self.new['Reference & Collections'])]:
            self.assertEqual(self.after.execute('SELECT concept_id FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi)).fetchone(),(id,))
        self.assertIn(5310,self.parents(5288))
        self.assertEqual(self.parents(51562),{self.new['Reference & Collections']})

    def test_merges_preserve_children_and_source_labels(self):
        for old,new in self.merges.items():
            self.assertIsNone(self.after.execute('SELECT 1 FROM concept WHERE concept_id=?',(old,)).fetchone())
            before={r[0] for r in self.before.execute('SELECT label FROM source_label WHERE concept_id=?',(old,))}
            after={r[0] for r in self.after.execute('SELECT label FROM source_label WHERE concept_id=?',(self.canonical(new),))}
            self.assertLessEqual(before,after)
        for id in (5059,5060,12979,12980,12981,12982):
            if self.before.execute('SELECT 1 FROM concept_parent WHERE concept_id=? AND parent_concept_id=5061',(id,)).fetchone():
                self.assertIn(5058,self.parents(id))
        self.assertEqual(self.parents(5493),{5662,5384})
        self.assertEqual(self.parents(5239),{5238,5664})
        self.assertEqual(self.parents(5496),{5664,self.new['Life Stages']})

    def test_anthropology_sociology_and_archaeology(self):
        self.assertEqual(self.parents(5072),{5488})
        self.assertEqual(self.label(5075),'Race & Race Relations')
        self.assertIn(5663,self.parents(5073))
        self.assertIn(5663,self.parents(5075))
        self.assertEqual(self.parents(5071),{5492})
        self.assertEqual(self.parents(5492),{5055,5056})

    def test_family_hierarchy_and_geography(self):
        self.assertEqual(self.parents(5252),{5244})
        for id in (5245,5248,5249,5250,5253):self.assertEqual(self.parents(id),{self.new['Gender & Sexuality']})
        for id in (12281,12282,12283,12284):self.assertEqual(self.parents(id),{self.new['Life Stages']})
        self.assertEqual(self.parents(12294),{self.new['Life Stages'],5664})
        self.assertEqual(self.parents(52556),{5252})
        self.assertEqual(self.parents(52557),{52556})
        self.assertEqual(self.parents(52558),{52557})
        for id in (5504,5510,5522):self.assertEqual(self.parents(id),{5462})

    def test_penology_is_broader_than_prisons(self):
        self.assertEqual(self.parents(5325),{5463})
        self.assertEqual(self.parents(5465),{5325,5472})
        for id in (52813,52814,52816,52817):self.assertEqual(self.parents(id),{5325})

    def test_psychology_groups_and_distinct_fields(self):
        for id in (7018,7688,9977,10805,5390):self.assertEqual(self.parents(id),{5389})
        for id in (10863,10896,10913,10975,10982,11212):self.assertEqual(self.parents(id),{5414})
        for id in (10895,10927,10968,11130,11199):self.assertEqual(self.parents(id),{self.new['Professional Practice']})
        self.assertEqual(self.parents(11019),{5385})
        self.assertEqual(self.parents(10990),{5418})
        self.assertEqual(self.label(10990),'Whole & Parts')
        for id in (5387,5422,5429):self.assertIn(5381,self.parents(id))
        self.assertEqual(self.parents(5410),{5384})

    def test_reference_and_context_labels(self):
        for id,label in [(5498,'Essays'),(5509,'Methods'),(12245,'Reference'),(12246,'Research')]:
            self.assertEqual(self.parents(id),{self.new['Methods & Reference']})
            self.assertEqual(self.label(id),label)
        for id,label in [(5057,'Fields'),(5071,'Prehistoric'),(5407,'History'),(52556,'By Region'),
                         (52557,'North America'),(52558,'United States'),(52940,'United States'),(5261,'Support Services')]:
            self.assertEqual(self.label(id),label)

    def test_previous_politics_cleanup_is_preserved(self):
        q='WITH RECURSIVE t(id) AS(VALUES(5326) UNION SELECT concept_id FROM concept_parent JOIN t ON parent_concept_id=id) SELECT id FROM t'
        ids={r[0] for r in self.before.execute(q)}
        self.assertEqual(ids,{r[0] for r in self.after.execute(q)})
        for table in ('concept','lcc_selector','lcc_range','bisac_selector','ddc_selector'):
            marks=','.join('?' for _ in ids)
            query=f'SELECT * FROM {table} WHERE concept_id IN({marks})'
            self.assertEqual(set(self.before.execute(query,tuple(ids))),set(self.after.execute(query,tuple(ids))))
    def test_materialized_and_simplified_ranges(self):
        self.assertEqual(len(self.manifest['added_matching_ranges']),16)
        for id,lo,hi in self.manifest['added_matching_ranges']:
            self.assertEqual(self.after.execute('SELECT concept_id FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi)).fetchone(),(id,))
        for id,lo,hi,outer_lo,outer_hi in self.manifest['removed_contained_ranges']:
            self.assertIsNone(self.after.execute('SELECT 1 FROM lcc_range WHERE start_code=? AND end_code=?',(lo,hi)).fetchone())
            self.assertEqual(self.after.execute('SELECT concept_id FROM lcc_range WHERE start_code=? AND end_code=?',(outer_lo,outer_hi)).fetchone(),(id,))
        summary=json.loads((FOLDER/'simplification-summary.json').read_text())
        self.assertGreater(summary['probes'],100000)
        self.assertEqual(summary['changes'],0)
        self.assertEqual((FOLDER/'simplification-changes.tsv').stat().st_size,0)

    def test_production_matching(self):
        reference=self.new['Reference & Collections']
        expected={'ddc:152':5381,'ddc:153':5389,'ddc:154':53287,'ddc:304':5488,'ddc:381':5083,'ddc:655':None,'ddc:658':5236,
            'ddc:381.1':5083,'ddc:381.2':5083,'ddc:304.6':5488,'ddc:152.4':5381,'ddc:154.6':53287,'ddc:655.1':None,
            'lcc:GN305':5058,'lcc:GN303.5':12979,'lcc:GN420':13198,'lcc:GN50':13001,'lcc:GN705':12847,
            'lcc:BF350':12875,'lcc:BF442':13040,'lcc:HT610':5073,'lcc:HT104':12299,'lcc:HT101 .Y67ax':12299,'lcc:HT900':12240,
            'lcc:HV8302':5325,'lcc:HV9500':5465,'lcc:K630':5310,'lcc:K680':5288,'lcc:K610':11731,
            'lcc:K40':reference,'lcc:K52':reference,'lcc:K69':reference,'lcc:VK555':478,
            'lcc:HD':5104,'lcc:HD75':13108,'lcc:BF204':5419,'lcc:BF202':10990,
            'bisac:SOC002010':5058,'bisac:SOC002020':5069,'bisac:SOC006000':5239,'bisac:SOC036000':5496,
            'bisac:PSY013000':5398,'bisac:SOC061000':5493,'bisac:SOC030000':5325}
        result=subprocess.run([str(migration.ROOT/'target/release/taxonomy_probe'),str(FOLDER/'after.sqlite3'),'--code-only',*expected],check=True,text=True,capture_output=True)
        actual={r['system']+':'+r['code']:r['resolved'] for r in csv.DictReader(io.StringIO(result.stdout))}
        self.assertEqual(actual,{k:'' if v is None else str(v) for k,v in expected.items()})

    def test_full_corpus_audit_is_complete_and_coverage_retained(self):
        # The audit writes this file only after the production runner exits 0.
        rows=json.loads((FOLDER/'full-nonmerge-changes.json').read_text())
        self.assertTrue(rows)
        self.assertTrue((FOLDER/'full-counts.tsv').stat().st_size)
        with sqlite3.connect(FOLDER/'matcher-audited.sqlite3') as audited:
            self.assertEqual(migration.snapshot(audited),migration.snapshot(self.after))
        for row in rows:self.assertTrue(row['after'],row)
        losses=[]
        with (FOLDER/'selectors-changes.tsv').open() as source:
            for system,uses,code,old,new in csv.reader(source,delimiter='\t'):
                if old and not new:losses.append((system,bytes.fromhex(code).decode()))
        self.assertEqual(losses,[('ddc','655')])

    def test_all_full_corpus_destination_changes_are_reviewed(self):
        rows=json.loads((FOLDER/'full-nonmerge-changes.json').read_text())
        # Expected direct code corrections and newly operational child ranges.
        direct={
            ((5157,),(5104,)):'HD', ((13770,),(478,)):'VK',
            ((5555,),(self.new['Reference & Collections'],)):'K',
            ((5310,),(5288,)):'K', ((5056,),(5058,)):'GN',
            ((5056,),(12979,)):'GN', ((5056,),(5071,)):'GN',
            ((5056,),(5069,)):'GN', ((5056,),(5059,)):'GN',
            ((5472,),(5325,)):'HV', ((5472,),(5465,)):'HV',
            ((5465,),(5325,)):'HV',
        }
        direct.update({((),(id,)):'HT' for id in (5073,12240,12299,5077)})
        direct.update({((5072,),(id,)):'HT' for id in (5073,12240,12299)})
        # Explicitly reviewed noisy imports: common-parent recovery and ancestor
        # suppression legitimately change when Demography, family and Penology
        # gain their corrected parents. Preserve this exact exception list;
        # do not silently accept arbitrary changed destinations.
        recovery={}
        def allow(codes,old,new):
            for code in codes:recovery[code]=(tuple(old),tuple(new))
        allow(['BJ1-1725BJB65HV8301-','HV8699+','HV9110*','HV9309*','HV9742.5+',
               'HV9785.2+','HV9795.5+','HV9800.6+','HV9802+'],[5472],[5463])
        allow(['D1-DX301GN370HB1951-','H1-970.9GN370HB1951-','HD28-70HD28-70HB848-',
               'HD28-70HD30.23HB848-','JA1-92LC8-6691HB848-','JC479H96-H97.7HB846-',
               'JC479HV40-69.2HB846-','JZ2-6530GN370HB1951-','P40-40.5GN370HB1951-'],[5593],[])
        allow(['G1-922H1-970.9HB848-','HB121*','HB2096.4+','HB225*','HB2310.5+',
               'HB3531+','HB3636.9+','HB3647+'],[5593],[5055])
        allow(['GV199.42.A68 H24*','PS3560.O864 H6*','PS3563.A8598 H5*'],[5055],[self.new['Methods & Reference']])
        allow(['HD1401-2210.2HD87-HD','HT388HD28-9999HD21HC','HT388HD28-9999HD21HM'],[5157],[])
        allow(['HQ1063.2+','HQ1064*','HQ72*','HQ769*','HQ770.5*','HQ774*','HQ775*','HQ779*',
               'HQ781.5*','HQ784*','HQ797*','HQ799.15*','HQ799.2*','HQ839*','HQ922.56+',
               'HV6001-7220.5HQ1060-'],[5238],[5244])
        allow(['HT166*'],[5701],[])
        allow(['HT169*'],[597],[5077])
        allow(['HT388HD28-9999HB848-'],[5157,5593],[5104])
        allow(['HT919+'],[5075],[12240])
        allow(['HV6001-7220.5HV8301-','HV6001-7220.5HV9051-','HV6001-7220.5HV9261-'],[5472],[])
        allow(['Ht395*'],[5374],[])
        allow(['VK1023+','VK140*'],[5254],[458])
        seen=set()
        for row in rows:
            old={self.canonical(id) for id in row['before']}
            new=set(row['after'])
            delta=(tuple(sorted(old-new)),tuple(sorted(new-old)))
            if delta in direct:
                self.assertIn(direct[delta],row['code'].upper(),row)
                continue
            self.assertIn(row['code'],recovery,row)
            self.assertEqual(delta,recovery[row['code']],row)
            seen.add(row['code'])
        self.assertEqual(seen,set(recovery))

    @unittest.skipUnless(LIVE,'Use --live after book')
    def test_live_database_and_complete_viewer_payload(self):
        with sqlite3.connect(f'file:{migration.DATABASE}?mode=ro',uri=True) as live:
            self.assertEqual(migration.snapshot(live),migration.snapshot(self.after))
        viewer=(migration.DATABASE.parents[1]/'taxonomy-viewer.html').read_text()
        data,_=json.JSONDecoder().raw_decode(viewer.split('const DATA=',1)[1])
        nodes=data['nodes']
        labels=dict(self.after.execute('SELECT concept_id,preferred_label FROM concept'))
        self.assertEqual({int(id):n['l'] for id,n in nodes.items()},labels)
        edges={(child,int(parent)) for parent,n in nodes.items() for child in n['c']}
        self.assertEqual(edges,set(self.after.execute('SELECT concept_id,parent_concept_id FROM concept_parent')))
        with (FOLDER/'full-counts.tsv').open() as source:
            counts={int(id):int(count) for id,count in csv.reader(source,delimiter='\t')}
        for id,n in nodes.items():self.assertEqual(n['h'],counts.get(int(id),0))
        self.assertTrue(any('GN301' in code for code in nodes['5058']['q']))


if __name__=='__main__':unittest.main()
