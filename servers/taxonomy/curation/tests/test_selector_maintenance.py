import contextlib
import csv
import io
import runpy
import sqlite3
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1]
M = runpy.run_path(str(TOOLS / 'compile-curated-selectors.py'))
OBSERVED = runpy.run_path(str(TOOLS / 'attach-observed-lcc.py'))


def database(path=':memory:'):
    db = sqlite3.connect(path)
    db.executescript('''CREATE TABLE concept(concept_id INTEGER PRIMARY KEY, preferred_label TEXT);
        CREATE TABLE concept_parent(concept_id INTEGER,parent_concept_id INTEGER,ordinal INTEGER);
        CREATE TABLE source_selector(concept_id INTEGER,system_id TEXT,selector TEXT,
            PRIMARY KEY(concept_id,system_id,selector));''')
    return db


class SelectorMaintenanceTests(unittest.TestCase):
    def database(self, path=':memory:'):
        db = database(path)
        self.addCleanup(db.close)
        return db

    def test_maintenance_preserves_curated_decisions_and_is_idempotent(self):
        db = self.database()
        db.executemany('INSERT INTO source_selector VALUES(?,?,?)', [
            (1,'lcc','K'), (1,'lcc','QA1..QA10@3'), (1,'lcc','QA1..QA10'),
            (1,'lcc','QA2..QA8'), (1,'lcc','QA5.C6'), (2,'lcc','QA20'),
            (1,'ddc','510'), (1,'bisac','MAT000000')])
        db.commit()
        before = M['selector_rows'](db)
        M['apply'](db, M['compile_selectors'](db), before)
        after = M['selector_rows'](db)
        self.assertEqual(after, before - {(1,'lcc','QA1..QA10@3')})
        M['apply'](db, M['compile_selectors'](db), after)
        self.assertEqual(M['selector_rows'](db), after)

    def test_maintenance_is_idempotent_on_split_selector_tables(self):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        db.executescript('''CREATE TABLE lcc_selector(concept_id INTEGER,selector TEXT,
                PRIMARY KEY(concept_id,selector));
            CREATE TABLE bisac_selector(concept_id INTEGER,selector TEXT,
                PRIMARY KEY(concept_id,selector));''')
        db.executemany('INSERT INTO lcc_selector VALUES(?,?)',
            [(1,'K'),(1,'QA1..QA10@3'),(1,'QA1..QA10'),(2,'QA20')])
        db.execute("INSERT INTO bisac_selector VALUES(1,'MAT000000')")
        db.commit()
        before = M['selector_rows'](db)
        M['apply'](db, M['compile_selectors'](db), before)
        after = M['selector_rows'](db)
        self.assertEqual(after, before - {(1,'lcc','QA1..QA10@3')})
        M['apply'](db, M['compile_selectors'](db), after)
        self.assertEqual(M['selector_rows'](db), after)

    def test_master_proposals_respect_ranges_and_explicit_moves(self):
        curated, master = self.database(), self.database()
        for db in (curated, master):
            db.executemany('INSERT INTO concept VALUES(?,?)', [(1,'Mathematics'),(2,'Specific topic')])
            db.execute('INSERT INTO concept_parent VALUES(2,1,0)')
        curated.executemany('INSERT INTO source_selector VALUES(?,?,?)',
            [(1,'lcc','QA1..QA10'),(2,'lcc','QA20')])
        master.executemany('INSERT INTO source_selector VALUES(?,?,?)',
            [(1,'lcc',s) for s in ['QA5.C6','QA2..QA9@9','QA20','QA1-10','QA11']])
        proposals, _ = M['propose_master_additions'](master,curated)
        self.assertEqual({r[3] for r in proposals}, {'QA1-10','QA11'})

    def test_independent_concept_ids_are_not_treated_as_same_subject(self):
        curated, master = self.database(), self.database()
        curated.execute("INSERT INTO concept VALUES(9,'Poetry')")
        master.execute("INSERT INTO concept VALUES(9,'Criminal law')")
        master.execute("INSERT INTO source_selector VALUES(9,'lcc','K500')")
        proposals, unresolved = M['propose_master_additions'](master,curated)
        self.assertEqual(proposals, [])
        self.assertEqual(unresolved, 1)

    def test_range_gaps_are_not_treated_as_covered(self):
        curated, master = self.database(), self.database()
        for db in (curated,master):
            db.execute("INSERT INTO concept VALUES(1,'Law')")
        curated.executemany('INSERT INTO source_selector VALUES(?,?,?)',
            [(1,'lcc','K1..K5'),(1,'lcc','K10..K20')])
        master.execute("INSERT INTO source_selector VALUES(1,'lcc','K4..K11')")
        proposals,_ = M['propose_master_additions'](master,curated)
        self.assertEqual([r[3] for r in proposals], ['K4..K11'])

    def test_concurrent_selector_changes_are_preserved(self):
        db = self.database()
        before = M['selector_rows'](db)
        planned = M['compile_selectors'](db)
        db.execute("INSERT INTO source_selector VALUES(1,'lcc','QA5')")
        db.commit()
        with self.assertRaisesRegex(RuntimeError,'changed during planning'):
            M['apply'](db,planned,before)
        self.assertEqual(M['selector_rows'](db), {(1,'lcc','QA5')})

    def test_stale_observed_candidates_cannot_override_matched_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory)
            db = self.database(p/'taxonomy.sqlite3')
            db.execute("INSERT INTO concept VALUES(1,'Mathematics')")
            db.execute("INSERT INTO source_selector VALUES(1,'lcc','QA1..QA10')")
            db.commit(); db.close()
            ref = sqlite3.connect(p/'reference.sqlite3')
            ref.executescript('''CREATE TABLE reference_current_record(record_id INTEGER,caption TEXT);
                CREATE TABLE classification_span(record_id INTEGER,table_id TEXT,ordinal INTEGER,start_number TEXT,end_number TEXT);
                CREATE TABLE caption_hierarchy(record_id INTEGER,label TEXT,ordinal INTEGER);
                INSERT INTO reference_current_record VALUES(1,'Mathematics'),(2,'General works'),(3,'General works');
                INSERT INTO classification_span VALUES(1,'',0,'QA1','QA10'),(2,'',0,'QA5',NULL),(3,'',0,'QA6',NULL);
                INSERT INTO caption_hierarchy VALUES(2,'Mathematics',0),(3,'Mathematics',0);''')
            ref.close()
            (p/'coverage.csv').write_text('code,kind,concept_ids\nQA5,matched,1\nQA6,unmatched,\n')
            (p/'candidates.csv').write_text('lcc_string\nQA5\nQA6\n')
            with contextlib.redirect_stdout(io.StringIO()):
                OBSERVED['generate'](p/'taxonomy.sqlite3',p/'reference.sqlite3',p/'coverage.csv',p/'out',candidates=p/'candidates.csv')
            with (p/'out/proposals.csv').open() as f:
                self.assertEqual([r['lcc_code'] for r in csv.DictReader(f)], ['QA6'])


if __name__ == '__main__':
    unittest.main()
