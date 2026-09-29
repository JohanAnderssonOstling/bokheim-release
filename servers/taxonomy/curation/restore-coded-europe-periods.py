#!/usr/bin/env python3
"""Replace synthetic European periods with selector-backed master periods."""

from __future__ import annotations

import re
import sqlite3
from pathlib import Path


DATA = Path(__file__).resolve().parents[3] / 'shared/subject-projection' / "data"
CURATED = DATA / "unified-taxonomy-v2.sqlite3"
MASTER = DATA / "master-taxonomy-v2.sqlite3"

# IDs of the hand-authored chronological browse trees selected for this curation pass.
SYNTHETIC_IDS = set()

# Country trees wholly replaced by synthetic leaves. Other curated countries
# retain their selector-backed entries after their synthetic wrappers are
# flattened.
RESTORE_ROOTS = {
    "Bulgaria": 3545,
    "Denmark": 3580,
    "Finland": 3581,
    "Iceland": 3582,
    "Luxembourg": 3578,
    "Malta": 3574,
    "Netherlands": 3579,
    "Norway": 3583,
    "Romania": 3548,
}

YUGOSLAVIA_ROOT = 10171
YUGOSLAVIA_PERIODS = (
    (11553, "Late Yugoslavia, 1980–1992"),
    (11554, "Successor States, 1992–Present"),
)

SWEDEN_ROOT = 3584
SWEDEN_GAP_PERIODS = (
    (37431, "Early Kingdom, 1060–1134"),
    (11555, "High Middle Ages, 1134–1234"),
    (11556, "Age of Liberty & Gustavian Era, 1718–1818"),
    (11557, "Twentieth Century"),
)
SWEDEN_FALLBACK_CONCEPTS = (
    33855,  # Early/medieval
    33938,  # Modern
)

# These are topical subdivisions, events, campaigns, peoples, or individual
# rulers rather than useful broad chronological browse periods.
EXCLUDE = re.compile(
    r"foreign and general|political history|social life|military history|"
    r"special events|\brevolution|\brevolt|\bwars?\b|\binvasion|\bpeace of|"
    r"early works|chronicles|\bd\.\s*\d|\bkarl\b|\boskar\b|\bharald\b|"
    r"\bmagnus\b|\bsigurd\b|^carpi$|^dacians|^getae",
    re.IGNORECASE,
)


def master_periods(db: sqlite3.Connection, root_id: str) -> list[sqlite3.Row]:
    return db.execute(
        """
        WITH RECURSIVE descendants(id, under_period) AS (
          SELECT cp.concept_id, lower(c.preferred_label) = 'by period'
          FROM concept_parent cp
          JOIN concept c ON c.concept_id = cp.concept_id
          WHERE cp.parent_concept_id = ?
          UNION ALL
          SELECT cp.concept_id,
                 descendants.under_period OR lower(c.preferred_label) = 'by period'
          FROM descendants
          JOIN concept_parent cp ON cp.parent_concept_id = descendants.id
          JOIN concept c ON c.concept_id = cp.concept_id
        )
        SELECT DISTINCT c.concept_id, c.preferred_label
        FROM descendants
        JOIN concept c ON c.concept_id = descendants.id
        WHERE descendants.under_period
          AND EXISTS (
            SELECT 1 FROM source_selector s WHERE s.concept_id = c.concept_id
          )
        ORDER BY c.preferred_label, c.concept_id
        """,
        (root_id,),
    ).fetchall()


def main() -> None:
    curated = sqlite3.connect(CURATED)
    master = sqlite3.connect(MASTER)
    curated.row_factory = master.row_factory = sqlite3.Row
    curated.execute("PRAGMA foreign_keys=ON")
    removed = restored = selectors = 0

    with curated:
        # Flatten children of synthetic wrappers before removing every code-less
        # synthetic period. Repeat deepest-first so nested wrappers disappear.
        targets = [
            row[0]
            for row in curated.execute(
                "SELECT concept_id FROM concept WHERE NOT EXISTS "
                "(SELECT 1 FROM source_selector s WHERE s.concept_id=concept.concept_id)"
            )
            if row[0] in SYNTHETIC_IDS
        ]
        while targets:
            target_set = set(targets)
            leaf = next(
                (cid for cid in targets if not any(
                    row[0] in target_set
                    for row in curated.execute(
                        "SELECT concept_id FROM concept_parent WHERE parent_concept_id=?",
                        (cid,),
                    )
                )),
                targets[0],
            )
            parents = [r[0] for r in curated.execute(
                "SELECT parent_concept_id FROM concept_parent WHERE concept_id=?", (leaf,)
            )]
            children = [r[0] for r in curated.execute(
                "SELECT concept_id FROM concept_parent WHERE parent_concept_id=?", (leaf,)
            )]
            for child in children:
                for parent in parents:
                    if child == parent:
                        continue
                    ordinal = curated.execute(
                        "SELECT coalesce(max(ordinal)+1,0) FROM concept_parent WHERE concept_id=?",
                        (child,),
                    ).fetchone()[0]
                    curated.execute(
                        "INSERT OR IGNORE INTO concept_parent"
                        "(concept_id,parent_concept_id,ordinal) VALUES(?,?,?)",
                        (child, parent, ordinal),
                    )
            # `parent_concept_id` deliberately has no cascading delete: remove
            # the now-flattened outgoing edges explicitly first.
            curated.execute(
                "DELETE FROM concept_parent WHERE parent_concept_id=?", (leaf,)
            )
            curated.execute("DELETE FROM concept WHERE concept_id=?", (leaf,))
            targets.remove(leaf)
            removed += 1

        for country, root_id in RESTORE_ROOTS.items():
            # Merge duplicate master labels into one visible leaf while retaining
            # every selector associated with that named period.
            grouped: dict[str, list[sqlite3.Row]] = {}
            for period in master_periods(master, root_id):
                if not EXCLUDE.search(period["preferred_label"]):
                    grouped.setdefault(period["preferred_label"], []).append(period)

            for label, periods in grouped.items():
                concept_id = periods[0]["concept_id"]
                curated.execute(
                    "INSERT OR IGNORE INTO concept(concept_id,preferred_label) VALUES(?,?)",
                    (concept_id, label),
                )
                ordinal = curated.execute(
                    "SELECT coalesce(max(ordinal)+1,0) FROM concept_parent WHERE concept_id=?",
                    (concept_id,),
                ).fetchone()[0]
                curated.execute(
                    "INSERT OR IGNORE INTO concept_parent"
                    "(concept_id,parent_concept_id,ordinal) VALUES(?,?,?)",
                    (concept_id, root_id, ordinal),
                )
                restored += 1

                for period in periods:
                    for source in master.execute(
                        "SELECT system_id,selector FROM source_selector "
                        "WHERE concept_id=? ORDER BY system_id,selector",
                        (period["concept_id"],),
                    ):
                        # Codes relocated to the country fallback now regain their
                        # more precise period destination.
                        curated.execute(
                            "DELETE FROM source_selector WHERE concept_id=? "
                            "AND system_id=? AND selector=?",
                            (root_id, source["system_id"], source["selector"]),
                        )
                        before = curated.total_changes
                        curated.execute(
                            "INSERT OR IGNORE INTO source_selector"
                            "(concept_id,system_id,selector) VALUES(?,?,?)",
                            (concept_id, source["system_id"], source["selector"]),
                        )
                        selectors += curated.total_changes > before

        # Master has only two selector-backed periods at the Yugoslavia level;
        # its much larger chronological tree belongs to the successor countries.
        for order, (concept_id, label) in enumerate(YUGOSLAVIA_PERIODS, 10):
            curated.execute(
                "INSERT OR IGNORE INTO concept(concept_id,preferred_label) VALUES(?,?)",
                (concept_id, label),
            )
            curated.execute(
                "UPDATE concept SET preferred_label=? WHERE concept_id=?",
                (label, concept_id),
            )
            ordinal = curated.execute(
                "SELECT coalesce(max(ordinal)+1,0) FROM concept_parent WHERE concept_id=?",
                (concept_id,),
            ).fetchone()[0]
            curated.execute(
                "INSERT OR IGNORE INTO concept_parent"
                "(concept_id,parent_concept_id,ordinal) VALUES(?,?,?)",
                (concept_id, YUGOSLAVIA_ROOT, ordinal),
            )
            for source in master.execute(
                "SELECT system_id,selector FROM source_selector WHERE concept_id=?",
                (concept_id,),
            ):
                curated.execute(
                    "DELETE FROM source_selector WHERE concept_id=? AND system_id=? AND selector=?",
                    (YUGOSLAVIA_ROOT, source["system_id"], source["selector"]),
                )
                curated.execute(
                    "INSERT OR IGNORE INTO source_selector"
                    "(concept_id,system_id,selector) VALUES(?,?,?)",
                    (concept_id, source["system_id"], source["selector"]),
                )

        # Restore only the master periods needed to close Sweden's gaps. Broad
        # aggregate codes such as DL660 and DL701-DL879 remain at Sweden itself.
        for order, (concept_id, label) in enumerate(SWEDEN_GAP_PERIODS, 10):
            curated.execute(
                "INSERT OR IGNORE INTO concept(concept_id,preferred_label) VALUES(?,?)",
                (concept_id, label),
            )
            curated.execute(
                "UPDATE concept SET preferred_label=? WHERE concept_id=?",
                (label, concept_id),
            )
            ordinal = curated.execute(
                "SELECT coalesce(max(ordinal)+1,0) FROM concept_parent WHERE concept_id=?",
                (concept_id,),
            ).fetchone()[0]
            curated.execute(
                "INSERT OR IGNORE INTO concept_parent"
                "(concept_id,parent_concept_id,ordinal) VALUES(?,?,?)",
                (concept_id, SWEDEN_ROOT, ordinal),
            )
            for source in master.execute(
                "SELECT system_id,selector FROM source_selector WHERE concept_id=?",
                (concept_id,),
            ):
                curated.execute(
                    "DELETE FROM source_selector WHERE concept_id=? AND system_id=? AND selector=?",
                    (SWEDEN_ROOT, source["system_id"], source["selector"]),
                )
                curated.execute(
                    "INSERT OR IGNORE INTO source_selector"
                    "(concept_id,system_id,selector) VALUES(?,?,?)",
                    (concept_id, source["system_id"], source["selector"]),
                )

        for aggregate_id in SWEDEN_FALLBACK_CONCEPTS:
            for source in master.execute(
                "SELECT system_id,selector FROM source_selector WHERE concept_id=?",
                (aggregate_id,),
            ):
                curated.execute(
                    "INSERT OR IGNORE INTO source_selector"
                    "(concept_id,system_id,selector) VALUES(?,?,?)",
                    (SWEDEN_ROOT, source["system_id"], source["selector"]),
                )

    print(f"removed={removed} restored={restored} selectors={selectors}")


if __name__ == "__main__":
    main()
