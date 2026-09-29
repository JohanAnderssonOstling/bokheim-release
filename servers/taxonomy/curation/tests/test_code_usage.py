import contextlib
import io
import json
import runpy
import sqlite3
import tempfile
import unittest
from pathlib import Path


REBUILD = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'rebuild-code-usage.py'))['rebuild']


class CodeUsageTests(unittest.TestCase):
    def test_full_notations_survive_and_counts_are_conserved(self):
        with tempfile.TemporaryDirectory() as directory:
            source, output = Path(directory) / 'source.sqlite3', Path(directory) / 'usage.sqlite3'
            db = sqlite3.connect(source)
            db.executescript('''
                CREATE TABLE edition_classification(scheme INTEGER,notation TEXT);
                CREATE TABLE work_classification(scheme INTEGER,notation TEXT);
                INSERT INTO edition_classification VALUES
                    (2,' QA 76.73 .R3 2024 '),(2,'QA 76.73 .R3 2024'),
                    (2,'QA 76.73 .P98 2024'),(2,'QA75.5 - QA76.95'),
                    (2,'MLCS 2006/1234'),(1,'005.13 22');
                INSERT INTO work_classification VALUES (2,'QA 76.73 .R3 2024');
            ''')
            db.close()
            original = source.read_bytes()
            with contextlib.redirect_stdout(io.StringIO()):
                metadata = REBUILD(source, output)
            self.assertEqual(source.read_bytes(), original)
            with sqlite3.connect(output) as result:
                self.assertEqual(dict(result.execute('SELECT notation,uses FROM code_usage')), {
                    'QA 76.73 .R3 2024': 3, 'QA 76.73 .P98 2024': 1,
                    'QA75.5 - QA76.95': 1, 'MLCS 2006/1234': 1, '005.13 22': 1,
                })
                self.assertEqual(result.execute('SELECT SUM(uses) FROM code_usage').fetchone()[0], 7)
                self.assertEqual(json.loads(result.execute("SELECT value FROM cache_metadata WHERE key='normalization_version'").fetchone()[0]), 2)
            self.assertEqual(metadata['normalization_version'], 2)
            self.assertEqual(metadata['source_counts'], {
                'edition_classification': {1: 1, 2: 5}, 'work_classification': {2: 1},
            })
            with self.assertRaises(FileExistsError):
                REBUILD(source, output)


if __name__ == '__main__':
    unittest.main()
