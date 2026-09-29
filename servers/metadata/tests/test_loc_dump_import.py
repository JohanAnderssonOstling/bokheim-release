import gzip
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

MODULE = Path(__file__).resolve().parents[1] / 'import-loc-lcc.py'
spec = importlib.util.spec_from_file_location('loc_import', MODULE)
loc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loc)


def marc(record_id, fields, deleted=False):
    all_fields = [('001', record_id.encode())] + [(tag, b'  ' + b''.join(b'\x1f' + code.encode() + value.encode() for code, value in subs)) for tag, subs in fields]
    directory, data = b'', b''
    for tag, value in all_fields:
        value += b'\x1e'
        directory += f'{tag}{len(value):04d}{len(data):05d}'.encode()
        data += value
    base = 24 + len(directory) + 1
    length = base + len(data) + 1
    leader = f'{length:05d}{"d" if deleted else "n"}am a22{base:05d}   4500'.encode()
    assert len(leader) == 24
    return leader + directory + b'\x1e' + data + b'\x1d'


class ImportTests(unittest.TestCase):
    def test_no_isbn_records_are_searchable_and_updates_remove_stale_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); src = root/'source.sqlite'
            with sqlite3.connect(src) as db:
                db.executescript("CREATE TABLE snapshot(singleton INTEGER,dump_date TEXT,imported_at_ms INTEGER); INSERT INTO snapshot VALUES(1,'test',1);")
            raw = marc('old', [('245',[('a','Old title')]),('050',[('a','D1')])])
            raw += marc('old', [('245',[('a','Révolution française :'),('b','a history /')]),('100',[('a','Smith, Jane')]),('050',[('a','DC161'),('b','.S5 1900')])])
            raw += marc('gone', [('245',[('a','Deleted book')]),('050',[('a','D1')])])
            raw += marc('gone', [], deleted=True)
            raw += marc('no-code', [('245',[('a','No classification')])])
            raw += marc('invalid-isbn', [('245',[('a','Invalid ISBN book')]),('020',[('a','9780306406158')]),('050',[('a','QA76')])])
            dump = root/'part.utf8'; dump.write_bytes(raw)
            out = root/'out.sqlite'; result = loc.import_dump(src,out,[dump])
            self.assertEqual(result['lcc_records_without_isbn'],2)
            self.assertEqual(result['mappings'],0)
            with sqlite3.connect(out) as db:
                self.assertEqual(db.execute('SELECT count(*) FROM lc_record').fetchone()[0],3)
                self.assertEqual(db.execute('SELECT * FROM lc_record_lcc ORDER BY lc_record_id').fetchall(),[('invalid-isbn','QA76'),('old','DC161')])
                self.assertEqual(db.execute("SELECT title_key FROM lc_title WHERE lc_record_id='old' ORDER BY title_key").fetchall(),[('revolution francaise',),('revolution francaise a history',)])
                self.assertEqual(db.execute('SELECT count(*) FROM lc_record_isbn').fetchone()[0],0)

    def test_isbn_validation_and_qualifiers(self):
        self.assertEqual(loc.isbn13('0-306-40615-2 (hardcover)'), 9780306406157)
        self.assertEqual(loc.isbn13('080442957X'), 9780804429573)
        for value in ('9780306406158', '978030640615700', 'garbage', '1234567890'):
            self.assertIsNone(loc.isbn13(value))

    def test_resume_keeps_completed_parts_and_replays_partial_part(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); src = root/'source.sqlite'
            with sqlite3.connect(src) as db:
                db.executescript("CREATE TABLE snapshot(singleton INTEGER,schema_version INTEGER,dump_date TEXT,imported_at_ms INTEGER); INSERT INTO snapshot VALUES(1,8,'2026-07-31',1);")
            first, second = root/'first.utf8', root/'second.utf8'
            first.write_bytes(marc('lc1',[('020',[('a','9780306406157')]),('050',[('a','DC161')])]))
            second.write_bytes(marc('lc2',[('020',[('a','9780131103627')]),('050',[('a','QA76')])]))
            stage = root/'staging.sqlite'
            loc.import_dump(src,stage,[first])
            # Reproduce a committed complete part plus an interrupted partial part.
            with sqlite3.connect(stage) as db:
                db.execute('UPDATE snapshot SET imported_at_ms=1')
                db.execute("INSERT INTO isbn_lcc VALUES(9780131103627,'STALE','lc2')")
            result = loc.import_dump(src,root/'finished.sqlite',[first,second],resume_building=stage)
            self.assertEqual(result['records_read'],2)
            with sqlite3.connect(root/'finished.sqlite') as db:
                self.assertEqual(db.execute('SELECT notation FROM isbn_lcc ORDER BY notation').fetchall(),[('DC161',),('QA76',)])
            with self.assertRaises(ValueError):
                loc.import_dump(src,root/'wrong-source.sqlite',[first,second],resume_building=stage)

    def test_import_preserves_source_and_extracts_only_valid_isbns_and_050a(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root/'source.sqlite'
            with sqlite3.connect(src) as db:
                db.executescript("CREATE TABLE snapshot(singleton INTEGER, dump_date TEXT, imported_at_ms INTEGER); INSERT INTO snapshot VALUES(1,'2026-07-31',1); CREATE TABLE existing(value TEXT); INSERT INTO existing VALUES('unchanged');")
            before = src.read_bytes()
            raw = marc('lc1', [('020', [('a','0-306-40615-2 (cloth)'),('z','9780131103627')]), ('020',[('a','9780306406157')]), ('050',[('a','DC161'),('b','.C3 1989')]), ('050',[('a','DC38')]), ('245',[('a','Révolution française')])])
            raw += marc('deleted', [('020',[('a','9780131103627')]),('050',[('a','QA76')])])
            raw += marc('deleted', [], deleted=True)
            raw += marc('invalid', [('020',[('a','9780306406158')]),('050',[('a','QA76')])])
            raw += marc('metadata-only', [('020',[('a','9780131103627')]),('245',[('a','The C Programming Language')])])
            dump = root/'books.utf8.gz'
            with gzip.open(dump,'wb') as f: f.write(raw)
            out = root/'output.sqlite'
            result = loc.import_dump(src,out,[dump])
            self.assertEqual(result['mappings'],2)
            self.assertEqual(src.read_bytes(),before)
            with sqlite3.connect(out) as db:
                self.assertEqual(db.execute('SELECT isbn13,notation FROM isbn_lcc ORDER BY notation').fetchall(),[(9780306406157,'DC161'),(9780306406157,'DC38')])
                self.assertEqual(db.execute('SELECT * FROM existing').fetchall(),[('unchanged',)])
                self.assertEqual(db.execute('SELECT count(*) FROM lc_record').fetchone()[0],3)
                self.assertGreater(db.execute('SELECT imported_at_ms FROM snapshot').fetchone()[0],1)
            with self.assertRaises(FileExistsError):loc.import_dump(src,out,[dump])
            broken=root/'broken.utf8';broken.write_bytes(raw[:-3])
            with self.assertRaises(ValueError):loc.import_dump(src,root/'failed.sqlite',[broken])
            self.assertFalse((root/'failed.sqlite').exists())


if __name__ == '__main__':unittest.main()
