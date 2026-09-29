#!/usr/bin/env python3
"""Generate a self-contained HTML viewer for the curated taxonomy."""

import json
import re
import sqlite3
import csv
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATABASE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "unified-taxonomy-v2.sqlite3"
OUTPUT = HERE / "taxonomy-viewer.html"
USAGE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "taxonomy-usage-openlibrary-2026-07-31.tsv"
BISAC_SOURCE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "bisac-2021.csv"
LCC_SOURCE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "lcc-2024-outline.csv"


def readable_code_names():
    names = {}
    with BISAC_SOURCE.open(encoding="utf-8", newline="") as source:
        for row in csv.DictReader(source):
            names["BISAC " + row["Code"]] = row["Description"]
    with LCC_SOURCE.open(encoding="utf-8", newline="") as source:
        for row in csv.DictReader(source):
            start = f'{row["start_letters"]}{row["start_number"]}'
            end = f'{row["end_letters"]}{row["end_number"]}'
            names.setdefault("LCC " + start, row["path"])
            names.setdefault("LCC " + start + "–" + end, row["path"])
            names.setdefault("LCC " + start + "-" + end, row["path"])
            names.setdefault("LCC " + row["start_letters"], row["path"])
    # These curated Ancient Italy fragments are subdivisions of the single
    # LCC "History / General" row DG201-DG215.
    ancient_general = names.get("LCC DG201-DG215")
    if ancient_general:
        for selector in ("DG203-DG204", "DG205-DG206", "DG207-DG209", "DG214-DG215"):
            names["LCC " + selector] = ancient_general
    return names


def lcc_order_key(selector):
    """Same numeric/decimal Cutter ordering as runtime::lcc_order_key."""
    start = selector.strip().upper().split("..", 1)[0].split("-", 1)[0].rstrip("*").strip()
    if re.fullmatch(r"[A-Z]{1,4}", start):
        return (start, 0, "", ())
    match = re.match(r"([A-Z]{1,4})[\s']*(\d+)(?:\.(\d+))?", start)
    if not match:
        return None
    letters, whole, fraction = match.groups()
    rest, cutters = start[match.end():], []
    while True:
        rest = rest.lstrip(". \t\r\n")
        cutter = re.match(r"([A-Z]+)(\d*)", rest)
        if not cutter:
            break
        label, digits = cutter.groups()
        cutters.append((label, digits.rstrip("0") or ("0" if digits else "")))
        rest = rest[cutter.end():]
    return (letters, int(whole), (fraction or "").rstrip("0"), tuple(cutters))


def lcc_sort_orders(connection):
    keys = {}
    for concept_id, selector in connection.execute("SELECT concept_id,selector FROM viewer_lcc_selector"):
        key = lcc_order_key(selector)
        if key is not None and (concept_id not in keys or key < keys[concept_id]):
            keys[concept_id] = key
    ranks = {key: rank for rank, key in enumerate(sorted(set(keys.values())))}
    return {concept_id: ranks[key] for concept_id, key in keys.items()}


def period_dates(label):
    """A display-only chronological key; never change classification coverage."""
    text = label.casefold().replace('–', '-').replace('—', '-')
    years = list(re.finditer(r'(?<!\w)(\d{1,4})(?!\w)', text))
    century = re.search(r'(\d{1,2})(?:st|nd|rd|th)(?:\s*-\s*(\d{1,2})(?:st|nd|rd|th))?[ -]+centur', text)
    if century and not years:
        first, last = int(century[1]), int(century[2] or century[1])
        if re.search(r'\bb\.?c\.?e?\b', text):
            return (-first * 100, -(last - 1) * 100)
        return ((first - 1) * 100 + 1, last * 100)
    if not years:
        return None
    values = []
    for match in years[:2]:
        year = int(match[1])
        tail = text[match.end():]
        if re.match(r'\s*b\.?c', tail):
            year = -year
        values.append(year)
    # A shared BC suffix applies to both endpoints (500-100 BC).
    if len(values) == 2 and values[1] < 0 and not re.search(r'\b(?:ad|ce)\b', text[:years[1].start()]):
        values[0] = -abs(values[0])
    if re.search(r'\b(?:to|through|before|until)\s*$', text[:years[0].start()]):
        return (-100000, values[0])
    return (values[0], values[1] if len(values) > 1 else values[0])


# Sort anchors for otherwise undated entries; these are not display dates or
# changes to a subject's scope. Named eras without exact boundaries use broad
# ordering buckets below instead of pretending to have exact historical dates.
PERIOD_ANCHORS = {
    3528: (449, 1066), 3529: (1066, 1485),
    51154: (1898, 1898),
}

# Undated Mongolian phases follow their DS798.65 / .7 / .75 / .8 / .86
# sequence. Keep this relative instead of inventing boundaries for the labels.
PERIOD_MENU_ORDERS = {54754: [10635, 10289, 10752, 10738, 10517]}


def named_period_dates(label):
    text = label.casefold()
    if re.search(r'\b(earliest|early)\b', text) and not re.search(r'early modern', text):
        return (-100000, -100000)
    if re.search(r'ancient|antiquity|phoenician|roman period', text):
        return (-99999, 0)
    if re.search(r'medieval|middle ages', text):
        return (500, 1500)
    if re.search(r'renaissance|reformation', text):
        return (1400, 1600)
    if re.search(r'\bmodern\b', text):
        return (1500, 100000)
    return None


def history_period_orders(nodes, children):
    """Sort history period menus only; ordinary subject menus stay alphabetical."""
    history, pending = set(), [3319]
    while pending:
        cid = pending.pop()
        if cid not in history:
            history.add(cid)
            pending.extend(children.get(cid, []))
    groups = {cid for cid in history if nodes[cid]['l'].casefold() in
              ('by period', 'by era', 'eras', 'historical eras')}
    def dates(cid):
        node = nodes[cid]
        key = period_dates(node['l'])
        if key is not None:
            return key
        if cid in PERIOD_ANCHORS:
            return PERIOD_ANCHORS[cid]
        for original in node['o']:
            key = period_dates(original)
            if key is not None:
                return key
        return named_period_dates(node['l']) or next(
            (key for original in node['o'] if (key := named_period_dates(original)) is not None), None)
    orders = {}
    for parent in groups:
        orders[parent] = sorted(children[parent], key=lambda cid:
            (dates(cid) or (100000, 100000), nodes[cid]['l'].casefold(), cid))
        if parent in PERIOD_MENU_ORDERS:
            ranks = {cid: i for i, cid in enumerate(PERIOD_MENU_ORDERS[parent])}
            orders[parent].sort(key=lambda cid: ranks.get(cid, len(ranks)))
    return orders


def load_taxonomy():
    connection = sqlite3.connect(DATABASE)
    code_names = readable_code_names()
    # Format 6 stores selectors by system and numeric LCC ranges separately.
    # Temporary views let the reader also accept that layout without migrating it.
    if not connection.execute(
        "SELECT 1 FROM sqlite_master WHERE name='lcc_selector'"
    ).fetchone():
        connection.execute(
            "CREATE TEMP VIEW lcc_selector AS "
            "SELECT concept_id,selector FROM source_selector WHERE system_id='lcc' "
            "UNION ALL SELECT concept_id,class_letters || start_value || '..' || "
            "class_letters || end_value FROM lcc_range"
        )
        connection.execute(
            "CREATE TEMP VIEW bisac_selector AS "
            "SELECT concept_id,selector FROM source_selector WHERE system_id='bisac'"
        )
    # Both tables contain active curated assignments. Parent ranges (including
    # Law's broad coverage) remain active alongside exact selectors and children.
    connection.execute(
        "CREATE TEMP VIEW viewer_lcc_selector AS "
        "SELECT concept_id,selector FROM lcc_selector "
        "UNION ALL SELECT concept_id,start_code || '-' || end_code FROM main.lcc_range"
    )
    sort_orders = lcc_sort_orders(connection)
    direct_hits = {}
    if USAGE.exists():
        with USAGE.open(encoding="utf-8") as source:
            next(source, None)
            for line in source:
                concept_id, hits, _path = line.rstrip("\n").split("\t", 2)
                concept_id = int(concept_id)
                direct_hits[concept_id] = max(direct_hits.get(concept_id, 0), int(hits))
    selector_counts = dict(connection.execute(
        "SELECT concept_id, count(*) FROM ("
        "SELECT concept_id FROM viewer_lcc_selector UNION ALL "
        "SELECT concept_id FROM bisac_selector) GROUP BY concept_id"
    ))
    selector_values = {}
    for concept_id, selector in connection.execute(
        "SELECT concept_id, selector_value FROM ("
        "SELECT concept_id, 'LCC ' || selector AS selector_value FROM viewer_lcc_selector "
        "UNION ALL SELECT concept_id, 'BISAC ' || selector FROM bisac_selector) "
        "ORDER BY concept_id, selector_value"
    ):
        readable_name = code_names.get(selector)
        if readable_name is None and selector.startswith("LCC "):
            # Curated ranges can widen or combine several rows from the LCC
            # outline. Use the source row with the same starting code when an
            # exact range alias is unavailable.
            start_code = selector[4:].split("-", 1)[0]
            readable_name = code_names.get("LCC " + start_code)
        selector_values.setdefault(concept_id, []).append(
            f"{selector} — {readable_name}" if readable_name else selector
        )
    original_labels = {}
    for concept_id, label in connection.execute(
        "SELECT concept_id,label FROM source_label ORDER BY concept_id,ordinal"
    ):
        original_labels.setdefault(concept_id, []).append(label)
    all_nodes = {}
    for concept_id, label in connection.execute("SELECT concept_id, preferred_label FROM concept ORDER BY rowid"):
        all_nodes[concept_id] = {"l": label, "h": direct_hits.get(concept_id, 0), "s": selector_counts.get(concept_id, 0), "q": selector_values.get(concept_id, []), "o": original_labels.get(concept_id, [])}

    all_children = {concept_id: [] for concept_id in all_nodes}
    edges = []
    for child, parent in connection.execute(
        "SELECT concept_id, parent_concept_id FROM concept_parent ORDER BY parent_concept_id,concept_id"
    ):
        if child in all_nodes and parent in all_nodes:
            all_children[parent].append(child)
            edges.append((child, parent))

    nodes = all_nodes
    children = {concept_id: [] for concept_id in nodes}
    parented = set()
    for child, parent in edges:
        if child in nodes and parent in nodes:
            children[parent].append((nodes[child]["l"].casefold(), child))
            parented.add(child)
    for concept_id, values in children.items():
        values.sort()
        nodes[concept_id]["c"] = [value[1] for value in values]

    for concept_id, ordered in history_period_orders(nodes, all_children).items():
        nodes[concept_id]['c'] = ordered

    descendant_hits = {}
    def descendant_hit_count(concept_id):
        if concept_id not in descendant_hits:
            descendant_hits[concept_id] = sum(nodes[child]["h"] + descendant_hit_count(child) for child in nodes[concept_id]["c"])
        return descendant_hits[concept_id]
    for concept_id in nodes:
        nodes[concept_id]["b"] = descendant_hit_count(concept_id)

    roots = sorted((concept_id for concept_id in nodes if concept_id not in parented), key=lambda concept_id: (nodes[concept_id]["l"].casefold(), concept_id))
    connection.close()
    return {"nodes": nodes, "roots": roots}


def main():
    data = json.dumps(load_taxonomy(), ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    document = TEMPLATE.replace("__TAXONOMY_DATA__", data)
    OUTPUT.write_text(document, encoding="utf-8")
    print(f"wrote {OUTPUT}")


TEMPLATE = r'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Bokheim curated taxonomy</title>
<style>
:root{color-scheme:light;--paper:#f5f1e8;--ink:#24231f;--muted:#6c685f;--line:#d8d0c1;--accent:#315f52;--card:#fffdf8}
*{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.45 system-ui,sans-serif}
header{position:sticky;top:0;z-index:2;padding:22px clamp(18px,4vw,52px);background:rgba(245,241,232,.95);border-bottom:1px solid var(--line);backdrop-filter:blur(12px)}
h1{margin:0 0 4px;font:600 clamp(24px,4vw,38px)/1.1 Georgia,serif}header p{margin:0;color:var(--muted)}
.search{display:flex;gap:10px;margin-top:16px}input{width:min(720px,100%);padding:11px 14px;border:1px solid var(--line);border-radius:8px;background:var(--card);font:inherit;outline:none}input:focus{border-color:var(--accent);box-shadow:0 0 0 3px #315f5220}
main{height:calc(100vh - 137px);padding:20px clamp(16px,3vw,36px) 28px;display:flex;flex-direction:column}.status{margin-bottom:12px;color:var(--muted);flex:none}
.browser{display:flex;gap:12px;min-height:0;flex:1;overflow-x:auto;overflow-y:hidden;padding-bottom:8px}.column{flex:0 0 min(340px,82vw);height:100%;overflow-y:auto;border:1px solid var(--line);border-radius:10px;background:var(--card);padding:7px}.column-title{position:sticky;top:-7px;z-index:1;margin:-7px -7px 6px;padding:11px 12px;border-bottom:1px solid var(--line);background:var(--card);font-weight:600;color:var(--muted)}
.entry{display:flex;align-items:center;gap:8px;width:100%;min-height:44px;padding:7px 9px;border:0;border-radius:7px;background:transparent;color:var(--ink);font:inherit;text-align:left;cursor:pointer}.entry:hover{background:#f1ece1}.entry.selected{background:#e4ede8;color:#173f34}.entry.leaf{cursor:default}.entry.leaf:hover{background:transparent}.entry-label{flex:1}.original,.codes{display:block;color:var(--muted);font-size:10px;line-height:1.3}.original{font-style:italic}.metrics{display:flex;gap:5px;color:var(--muted);font-size:10px;font-variant-numeric:tabular-nums;white-space:nowrap}.metric{padding:2px 4px;border:1px solid var(--line);border-radius:4px}.chevron{color:var(--accent);font-size:18px}.match{padding:10px 12px;margin:5px 0;border:1px solid var(--line);border-radius:8px;background:var(--card);cursor:pointer}.match:hover{border-color:var(--accent)}
mark{background:#e7dca7}.empty{color:var(--muted);padding:30px 10px}@media(max-width:600px){main{height:calc(100vh - 157px);padding-inline:12px}.column{flex-basis:86vw}}
</style>
</head>
<body>
<header><h1>Curated taxonomy</h1><p>OpenLibrary 2026-07-31: C = child book hits, D = direct book hits, # = LCC and BISAC selector codes. Original source names appear below curated names.</p><div class="search"><input id="search" type="search" placeholder="Search subjects, original names, IDs, or codes…" autocomplete="off"></div></header>
<main><div class="status" id="status"></div><div id="content" class="browser"></div></main>
<script>
const DATA=__TAXONOMY_DATA__,nodes=DATA.nodes,content=document.querySelector('#content'),status=document.querySelector('#status'),search=document.querySelector('#search'),stateKey='bokheim-taxonomy-path-v1';
let currentPath=[];
const esc=value=>value.replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
function addMetrics(entry,n){const metrics=document.createElement('span');metrics.className='metrics';for(const [label,value,title] of [['C',n.b,'Child book hits'],['D',n.h,'Direct book hits'],['#',n.s,'LCC and BISAC selector codes']]){const metric=document.createElement('span');metric.className='metric';metric.title=title;metric.textContent=`${label} ${value.toLocaleString()}`;metrics.append(metric)}entry.append(metrics)}
function addDetails(label,n){if(n.o.length&&!(n.o.length===1&&n.o[0]===n.l)){const original=document.createElement('small');original.className='original';original.textContent=`Original: ${n.o.join(' · ')}`;label.append(original)}if(n.q.length){const codes=document.createElement('small');codes.className='codes';codes.textContent=n.q.join(' · ');label.append(codes)}}
function codeTitle(n){return n.q.length?`${n.l} — ${n.q.join(', ')}`:n.l}
function savePath(){localStorage.setItem(stateKey,JSON.stringify(currentPath))}
function validPath(path){let valid=[],choices=DATA.roots;for(const raw of Array.isArray(path)?path:[]){const id=String(raw);if(!choices.includes(Number(id))||!nodes[id])break;valid.push(id);choices=nodes[id].c}return valid}
function addColumn(ids,title,afterIndex){while(content.children.length>afterIndex+1)content.lastElementChild.remove();const column=document.createElement('section');column.className='column';const heading=document.createElement('div');heading.className='column-title';heading.textContent=title;column.append(heading);if(!ids.length){const empty=document.createElement('div');empty.className='empty';empty.textContent='No children';column.append(empty)}for(const id of ids){const n=nodes[id],entry=document.createElement('button');entry.dataset.id=id;entry.className=`entry${n.c.length?'':' leaf'}`;entry.title=codeTitle(n);const label=document.createElement('span');label.className='entry-label';label.textContent=n.l;addDetails(label,n);entry.append(label);addMetrics(entry,n);if(n.c.length){const arrow=document.createElement('span');arrow.className='chevron';arrow.textContent='›';entry.append(arrow);entry.onclick=()=>{for(const other of column.querySelectorAll('.entry.selected'))other.classList.remove('selected');entry.classList.add('selected');currentPath=currentPath.slice(0,afterIndex+1);currentPath.push(String(id));savePath();addColumn(n.c,n.l,afterIndex+1)}}column.append(entry)}content.append(column);requestAnimationFrame(()=>column.scrollIntoView({behavior:'smooth',block:'nearest',inline:'end'}));return column}
function showBrowser(){content.replaceChildren();currentPath=validPath(currentPath);let column=addColumn(DATA.roots,'Subjects',-1);for(const id of currentPath){const entry=column.querySelector(`[data-id="${id}"]`);if(!entry)break;entry.classList.add('selected');if(!nodes[id].c.length)break;column=addColumn(nodes[id].c,nodes[id].l,content.children.length-1)}savePath();status.textContent=`${Object.keys(nodes).length.toLocaleString()} subjects`}
function showSearch(query){const q=query.trim().toLocaleLowerCase();if(!q){showBrowser();return}const found=Object.entries(nodes).filter(([id,n])=>id.toLocaleLowerCase().includes(q)||n.l.toLocaleLowerCase().includes(q)||n.o.some(label=>label.toLocaleLowerCase().includes(q))||n.q.some(code=>code.toLocaleLowerCase().includes(q))).slice(0,250);content.replaceChildren();const column=document.createElement('section');column.className='column';const heading=document.createElement('div');heading.className='column-title';heading.textContent='Search results';column.append(heading);for(const [id,n] of found){const item=document.createElement('button');item.className=`entry${n.c.length?'':' leaf'}`;item.title=codeTitle(n);const label=document.createElement('span');label.className='entry-label';label.textContent=n.l;addDetails(label,n);item.append(label);addMetrics(item,n);if(n.c.length){const arrow=document.createElement('span');arrow.className='chevron';arrow.textContent='›';item.append(arrow);item.onclick=()=>{for(const other of column.querySelectorAll('.entry.selected'))other.classList.remove('selected');item.classList.add('selected');addColumn(n.c,n.l,0)}}column.append(item)}if(!found.length){const empty=document.createElement('div');empty.className='empty';empty.textContent='No matching subjects.';column.append(empty)}content.append(column);status.textContent=`${found.length}${found.length===250?' or more':''} matches`}
let timer;search.addEventListener('input',()=>{clearTimeout(timer);timer=setTimeout(()=>showSearch(search.value),100)});try{currentPath=JSON.parse(localStorage.getItem(stateKey)||'[]')}catch{currentPath=[]}showBrowser();
</script>
</body></html>'''


if __name__ == "__main__":
    main()
