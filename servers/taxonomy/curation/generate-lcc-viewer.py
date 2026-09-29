#!/usr/bin/env python3
"""Generate a self-contained viewer for the original LCC outline."""

import csv
import json
import re
import sqlite3
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "lcc-mdsconnect-2016.sqlite3"
OUTPUT = HERE / "lcc-viewer.html"


def load_outline():
    by_path = {}

    class_prefixes = {}
    caption_prefixes = {}
    with (HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "lcc-2024-outline.csv").open(encoding="utf-8", newline="") as source:
        for row in csv.DictReader(source):
            path_parts = [part.strip() for part in row["path"].split(" / ")]
            class_prefixes.setdefault(row["start_letters"], tuple(path_parts[:2]))
            for index, part in enumerate(path_parts):
                caption_prefixes.setdefault(part, set()).add(tuple(path_parts[:index]))
    caption_prefixes = {
        caption: next(iter(prefixes))
        for caption, prefixes in caption_prefixes.items()
        if len(prefixes) == 1
    }

    connection = sqlite3.connect(SOURCE)
    rows = connection.execute(
        """
        SELECT hierarchy_json, caption, start_number, end_number
        FROM reference_entry
        WHERE status != 'obsolete' AND json_valid(hierarchy_json)
        ORDER BY entry_id
        """
    )
    root_keys = set()
    for hierarchy_json, caption, start_number, end_number in rows:
        parts = [part.strip() for part in json.loads(hierarchy_json) if part.strip()]
        caption = caption.strip()
        if len(parts) > 1 and parts[0] == "National biography" and parts[1] in {
            "Africa", "America", "Asia", "Europe", "Oceania. Pacific islands"
        }:
            parts.insert(1, "By region or country")
        if caption and (not parts or parts[-1] != caption):
            parts.append(caption)
        if not parts:
            continue
        code_match = re.match(r"[A-Z]+", start_number or "")
        class_prefix = class_prefixes.get(code_match.group(0)) if code_match else None
        if class_prefix is None:
            class_prefix = caption_prefixes.get(parts[0])
        # MDSConnect also contains detached cutter/table pattern records such
        # as .A or .x. They have no class-level anchor and must not become
        # artificial roots in the navigable LCC hierarchy.
        if not class_prefix:
            continue
        if tuple(parts[:len(class_prefix)]) != class_prefix:
            if class_prefix[-1] == parts[0]:
                parts = list(class_prefix[:-1]) + parts
            else:
                parts = list(class_prefix) + parts
        parent = None
        for depth, label in enumerate(parts):
            key = tuple(parts[: depth + 1])
            node = by_path.setdefault(key, {"l": label, "r": [], "children": {}})
            if parent is None:
                root_keys.add(key)
            else:
                by_path[parent]["children"][label] = node
            parent = key
        if start_number:
            last = end_number or start_number
            by_path[tuple(parts)]["r"].append(
                f"{start_number}–{last}" if start_number != last else start_number
            )

    # Structural captions without their own call-number entry are stored in
    # caption_hierarchy. Add them so numbered descendants remain connected to
    # their actual LCC parents (for example National biography -> America).
    structural = connection.execute(
        """
        SELECT r.record_id, r.caption, h.ordinal, h.label
        FROM classification_record r
        JOIN caption_hierarchy h ON h.record_id = r.record_id
        LEFT JOIN reference_record_state s ON s.record_id = r.record_id
        WHERE r.scheme = 'lcc' AND coalesce(s.status, 'active') != 'obsolete'
        ORDER BY r.record_id, h.ordinal
        """
    )
    current_id = None
    hierarchy = []
    for record_id, caption, ordinal, label in structural:
        if current_id is not None and record_id != current_id:
            structural_path = [part.strip() for part in hierarchy if part.strip()]
            structural_caption = (previous_caption or "").strip()
            if structural_caption and (not structural_path or structural_path[-1] != structural_caption):
                structural_path.append(structural_caption)
            if structural_path:
                class_prefix = caption_prefixes.get(structural_path[0])
                if class_prefix:
                    if tuple(structural_path[:len(class_prefix)]) != class_prefix:
                        if class_prefix[-1] == structural_path[0]:
                            structural_path = list(class_prefix[:-1]) + structural_path
                        else:
                            structural_path = list(class_prefix) + structural_path
                    parent = None
                    for index, label in enumerate(structural_path):
                        key = tuple(structural_path[:index + 1])
                        node = by_path.setdefault(key, {"l": label, "r": [], "children": {}})
                        if parent is None:
                            root_keys.add(key)
                        else:
                            by_path[parent]["children"][label] = node
                        parent = key
        if record_id != current_id:
            current_id = record_id
            hierarchy = []
            previous_caption = caption or ""
        hierarchy.append(label)

    if current_id is not None:
        structural_path = [part.strip() for part in hierarchy if part.strip()]
        structural_caption = (previous_caption or "").strip()
        if structural_caption and (not structural_path or structural_path[-1] != structural_caption):
            structural_path.append(structural_caption)
        class_prefix = caption_prefixes.get(structural_path[0]) if structural_path else None
        if class_prefix:
            if tuple(structural_path[:len(class_prefix)]) != class_prefix:
                structural_path = list(class_prefix[:-1]) + structural_path if class_prefix[-1] == structural_path[0] else list(class_prefix) + structural_path
            parent = None
            for index, label in enumerate(structural_path):
                key = tuple(structural_path[:index + 1])
                node = by_path.setdefault(key, {"l": label, "r": [], "children": {}})
                if parent is None:
                    root_keys.add(key)
                else:
                    by_path[parent]["children"][label] = node
                parent = key
    roots = [by_path[key] for key in root_keys]
    connection.close()

    def finish(node):
        children = sorted(node["children"].values(), key=lambda child: child["l"].casefold())
        node["c"] = [finish(child) for child in children]
        result = {"l": node["l"], "r": sorted(set(node["r"]))}
        if node["c"]:
            result["c"] = node["c"]
        return result

    return {"c": [finish(root) for root in sorted(roots, key=lambda root: root["l"].casefold())]}


TEMPLATE = r'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Complete LCC hierarchy</title>
<style>
:root{color-scheme:light;--paper:#f5f1e8;--ink:#24231f;--muted:#6c685f;--line:#d8d0c1;--accent:#315f52;--card:#fffdf8}
*{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.45 system-ui,sans-serif}
header{position:sticky;top:0;z-index:2;padding:22px clamp(18px,4vw,52px);background:rgba(245,241,232,.95);border-bottom:1px solid var(--line);backdrop-filter:blur(12px)}
h1{margin:0 0 4px;font:600 clamp(24px,4vw,38px)/1.1 Georgia,serif}header p{margin:0;color:var(--muted)}.search{display:flex;margin-top:16px}input{width:min(720px,100%);padding:11px 14px;border:1px solid var(--line);border-radius:8px;background:var(--card);font:inherit;outline:none}
main{height:calc(100vh - 137px);padding:20px clamp(16px,3vw,36px) 28px;display:flex;flex-direction:column}.status{margin-bottom:12px;color:var(--muted);flex:none}.browser{display:flex;gap:12px;min-height:0;flex:1;overflow:auto;padding-bottom:8px}.column{flex:0 0 min(380px,86vw);height:100%;overflow-y:auto;border:1px solid var(--line);border-radius:10px;background:var(--card);padding:7px}.column-title{position:sticky;top:-7px;margin:-7px -7px 6px;padding:11px 12px;border-bottom:1px solid var(--line);background:var(--card);font-weight:600;color:var(--muted)}
.entry{display:flex;align-items:flex-start;gap:8px;width:100%;min-height:44px;padding:8px 9px;border:0;border-radius:7px;background:transparent;color:var(--ink);font:inherit;text-align:left;cursor:pointer}.entry:hover,.entry.selected{background:#e4ede8}.label{flex:1}.range{display:block;color:var(--muted);font-size:11px}.arrow{color:var(--accent);font-size:18px}.empty{color:var(--muted);padding:30px 10px}.match{padding:10px 12px;margin:5px 0;border:1px solid var(--line);border-radius:8px;background:var(--card);cursor:pointer}.match small{display:block;color:var(--muted)}
</style></head><body><header><h1>Complete LCC hierarchy</h1><p>Library of Congress Classification captions and ranges, generated from the local MDSConnect reference source.</p><div class="search"><input id="search" type="search" placeholder="Search titles or LCC ranges…" autocomplete="off"></div></header><main><div class="status" id="status"></div><div id="content" class="browser"></div></main>
<script>
const DATA=__LCC_DATA__,content=document.querySelector('#content'),status=document.querySelector('#status'),search=document.querySelector('#search');
function addColumn(nodes,title,after){while(content.children.length>after+1)content.lastElementChild.remove();const col=document.createElement('section');col.className='column';const h=document.createElement('div');h.className='column-title';h.textContent=title;col.append(h);if(!nodes.length){const e=document.createElement('div');e.className='empty';e.textContent='No subdivisions';col.append(e)}for(const n of nodes){const b=document.createElement('button');b.className='entry';const label=document.createElement('span');label.className='label';label.textContent=n.l;const range=document.createElement('small');range.className='range';range.textContent=n.r.join(' · ');label.append(range);b.append(label);if(n.c){const a=document.createElement('span');a.className='arrow';a.textContent='›';b.append(a);b.onclick=()=>{col.querySelectorAll('.selected').forEach(x=>x.classList.remove('selected'));b.classList.add('selected');addColumn(n.c,n.l,after+1)}}col.append(b)}content.append(col)}
function browse(){content.replaceChildren();addColumn(DATA.c,'LCC classes',-1);status.textContent='Complete LCC hierarchy'}
function searchFor(value){const q=value.trim().toLocaleLowerCase();if(!q){browse();return}const found=[];(function walk(nodes,path){for(const n of nodes){if((n.l+' '+n.r.join(' ')).toLocaleLowerCase().includes(q))found.push({n,path});if(n.c)walk(n.c,path.concat(n.l))}})(DATA.c,[]);content.replaceChildren();const col=document.createElement('section');col.className='column';const h=document.createElement('div');h.className='column-title';h.textContent='Search results';col.append(h);for(const item of found.slice(0,300)){const b=document.createElement('button');b.className='match';b.textContent=item.n.l;const small=document.createElement('small');small.textContent=item.n.r.join(' · ')+' — '+item.path.join(' / ');b.append(small);b.onclick=()=>{content.replaceChildren();addColumn([item.n],item.path.at(-1)||'Result',-1)};col.append(b)}if(!found.length){const e=document.createElement('div');e.className='empty';e.textContent='No matching LCC entries.';col.append(e)}content.append(col);status.textContent=`${Math.min(found.length,300)}${found.length>300?' or more':''} matches`}
let timer;search.addEventListener('input',()=>{clearTimeout(timer);timer=setTimeout(()=>searchFor(search.value),100)});browse();
</script></body></html>'''


def main():
    data = json.dumps(load_outline(), ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    OUTPUT.write_text(TEMPLATE.replace("__LCC_DATA__", data), encoding="utf-8")
    print(f"wrote {OUTPUT}")


if __name__ == "__main__":
    main()
