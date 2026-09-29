"""Export registered non-law LC table IDs for imported table-note recognition."""
import pathlib
import re
import sqlite3

root = pathlib.Path(__file__).resolve().parents[3] / 'shared/subject-projection'
source = root / "data/lcc-mdsconnect-2016.sqlite3"
with sqlite3.connect(f"file:{source}?mode=ro", uri=True) as database:
    identifiers = sorted({
        row[0].upper()
        for row in database.execute("SELECT DISTINCT table_id FROM classification_span WHERE table_id IS NOT NULL")
        if re.fullmatch(r"[A-Z]+(?:-[A-Z]+)?[0-9]+[a-z]?", row[0])
        and not row[0].startswith("K")
    })
(root / "data/lcc-table-identifiers.txt").write_text("\n".join(identifiers) + "\n")
print(f"Exported {len(identifiers)} table identifiers")
