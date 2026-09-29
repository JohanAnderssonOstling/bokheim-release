import gzip
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('rank_missing_lcc', Path(__file__).with_name('rank-missing-lcc.py'))
ranker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ranker)


class SharedIsbnRankingTest(unittest.TestCase):
    def test_unrated_duplicate_with_lcc_excludes_rated_work_sharing_isbn(self):
        for table, key, record in [('edition_classification', 'edition_id', 20), ('work_classification', 'work_id', 2), ('edition_work_classification', 'work_id', 2)]:
            with self.subTest(table=table), tempfile.TemporaryDirectory() as temp:
                path = Path(temp) / 'snapshot.sqlite'
                db = sqlite3.connect(path)
                db.executescript('''
                    CREATE TABLE edition(edition_id INTEGER,work_id INTEGER);
                    CREATE TABLE edition_isbn(edition_id INTEGER,isbn13 INTEGER);
                    CREATE TABLE edition_classification(edition_id INTEGER,scheme INTEGER,notation TEXT);
                    CREATE TABLE work_classification(work_id INTEGER,scheme INTEGER,notation TEXT);
                    CREATE TABLE edition_work_classification(work_id INTEGER,scheme INTEGER,notation TEXT);
                    CREATE TABLE isbn_lcc(isbn13 INTEGER,notation TEXT);
                    CREATE TABLE edition_bibliography(edition_id INTEGER,title TEXT,book_year INTEGER);
                    CREATE TABLE work_bibliography(work_id INTEGER,title TEXT);
                    CREATE TABLE work_author(work_id INTEGER,author_id INTEGER,position INTEGER);
                    CREATE TABLE author(author_id INTEGER,name TEXT);
                    INSERT INTO edition VALUES(10,1),(20,2),(30,3);
                    INSERT INTO edition_isbn VALUES(10,9780743477116),(20,9780743477116),(30,9781538724736);
                    INSERT INTO edition_bibliography VALUES(10,'Romeo and Juliet',1595),(20,'The Tragedy of Romeo and Juliet',2002),(30,'Uncoded work',2024);
                    INSERT INTO work_bibliography VALUES(1,'Romeo and Juliet'),(2,'Romeo and Juliet'),(3,'Uncoded work');
                ''')
                db.execute(f'INSERT INTO {table}({key},scheme,notation) VALUES(?,2,?)', (record, 'PR2831'))
                db.commit()
                db.close()
                ratings = Path(temp) / 'ratings.gz'
                with gzip.open(ratings, 'wt') as stream:
                    stream.write('/works/OL1W\t\\N\t5\t2026-01-01\n' * 3)
                    stream.write('/works/OL3W\t\\N\t5\t2026-01-01\n')
                result = ranker.rank(path, ratings, None, 1, 2020, popular_only=True)
                self.assertEqual([r['work_id'] for r in result['popular']], ['OL3W'])


if __name__ == '__main__':
    unittest.main()
