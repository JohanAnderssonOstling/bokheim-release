#!/usr/bin/env python3
"""Generate a self-contained viewer for the original BISAC hierarchy."""

import csv
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE.resolve().parents[3] / 'shared/subject-projection' / "data" / "bisac-2021.csv"
OUTPUT = HERE / "bisac-viewer.html"


def load_outline():
    nodes = {}
    roots = set()
    with SOURCE.open(encoding="utf-8", newline="") as source:
        for row in csv.DictReader(source):
            parts = tuple(part.strip() for part in row["Description"].split(" / "))
            for depth, label in enumerate(parts):
                key = parts[: depth + 1]
                nodes.setdefault(key, {"l": label, "r": [], "children": set()})
                if depth == 0:
                    roots.add(key)
                else:
                    nodes[key[:-1]]["children"].add(key)
            nodes[parts]["r"].append(f'{row["Code"]} — {parts[-1]}')

    def finish(key):
        node = nodes[key]
        result = {"l": node["l"], "r": sorted(set(node["r"]))}
        children = sorted(node["children"], key=lambda child: (nodes[child]["l"].casefold(), child))
        if children:
            result["c"] = [finish(child) for child in children]
        return result

    return {"c": [finish(key) for key in sorted(roots, key=lambda root: nodes[root]["l"].casefold())]}


TEMPLATE = r'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Original BISAC hierarchy</title>
<style>
:root{color-scheme:light;--paper:#f5f1e8;--ink:#24231f;--muted:#6c685f;--line:#d8d0c1;--accent:#315f52;--card:#fffdf8}
*{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.45 system-ui,sans-serif}
header{position:sticky;top:0;z-index:2;padding:22px clamp(18px,4vw,52px);background:rgba(245,241,232,.95);border-bottom:1px solid var(--line);backdrop-filter:blur(12px)}
h1{margin:0 0 4px;font:600 clamp(24px,4vw,38px)/1.1 Georgia,serif}header p{margin:0;color:var(--muted)}.search{display:flex;margin-top:16px}input{width:min(720px,100%);padding:11px 14px;border:1px solid var(--line);border-radius:8px;background:var(--card);font:inherit;outline:none}
main{height:calc(100vh - 137px);padding:20px clamp(16px,3vw,36px) 28px;display:flex;flex-direction:column}.status{margin-bottom:12px;color:var(--muted);flex:none}.browser{display:flex;gap:12px;min-height:0;flex:1;overflow:auto;padding-bottom:8px}.column{flex:0 0 min(380px,86vw);height:100%;overflow-y:auto;border:1px solid var(--line);border-radius:10px;background:var(--card);padding:7px}.column-title{position:sticky;top:-7px;margin:-7px -7px 6px;padding:11px 12px;border-bottom:1px solid var(--line);background:var(--card);font-weight:600;color:var(--muted)}
.entry{display:flex;align-items:flex-start;gap:8px;width:100%;min-height:44px;padding:8px 9px;border:0;border-radius:7px;background:transparent;color:var(--ink);font:inherit;text-align:left;cursor:pointer}.entry:hover,.entry.selected{background:#e4ede8}.label{flex:1}.code{display:block;color:var(--muted);font-size:11px}.arrow{color:var(--accent);font-size:18px}.empty{color:var(--muted);padding:30px 10px}.match{padding:10px 12px;margin:5px 0;border:1px solid var(--line);border-radius:8px;background:var(--card);cursor:pointer}.match small{display:block;color:var(--muted)}
</style></head><body><header><h1>Original BISAC hierarchy</h1><p>Book Industry Study Group subject headings, 2021 edition.</p><div class="search"><input id="search" type="search" placeholder="Search headings or BISAC codes…" autocomplete="off"></div></header><main><div class="status" id="status"></div><div id="content" class="browser"></div></main>
<script>
const DATA=__BISAC_DATA__,content=document.querySelector('#content'),status=document.querySelector('#status'),search=document.querySelector('#search');
function addColumn(nodes,title,after){while(content.children.length>after+1)content.lastElementChild.remove();const col=document.createElement('section');col.className='column';const h=document.createElement('div');h.className='column-title';h.textContent=title;col.append(h);if(!nodes.length){const e=document.createElement('div');e.className='empty';e.textContent='No subdivisions';col.append(e)}for(const n of nodes){const b=document.createElement('button');b.className='entry';const label=document.createElement('span');label.className='label';label.textContent=n.l;const code=document.createElement('small');code.className='code';code.textContent=n.r.join(' · ');label.append(code);b.append(label);if(n.c){const a=document.createElement('span');a.className='arrow';a.textContent='›';b.append(a);b.onclick=()=>{col.querySelectorAll('.selected').forEach(x=>x.classList.remove('selected'));b.classList.add('selected');addColumn(n.c,n.l,after+1)}}col.append(b)}content.append(col)}
function browse(){content.replaceChildren();addColumn(DATA.c,'BISAC categories',-1);status.textContent='Original BISAC hierarchy'}
function searchFor(value){const q=value.trim().toLocaleLowerCase();if(!q){browse();return}const found=[];(function walk(nodes,path){for(const n of nodes){if((n.l+' '+n.r.join(' ')).toLocaleLowerCase().includes(q))found.push({n,path});if(n.c)walk(n.c,path.concat(n.l))}})(DATA.c,[]);content.replaceChildren();const col=document.createElement('section');col.className='column';const h=document.createElement('div');h.className='column-title';h.textContent='Search results';col.append(h);for(const item of found.slice(0,300)){const b=document.createElement('button');b.className='match';b.textContent=item.n.l;const small=document.createElement('small');small.textContent=item.n.r.join(' · ')+' — '+item.path.join(' / ');b.append(small);col.append(b)}if(!found.length){const e=document.createElement('div');e.className='empty';e.textContent='No matching BISAC entries.';col.append(e)}content.append(col);status.textContent=`${Math.min(found.length,300)}${found.length>300?' or more':''} matches`}
let timer;search.addEventListener('input',()=>{clearTimeout(timer);timer=setTimeout(()=>searchFor(search.value),100)});browse();
</script></body></html>'''


def main():
    data = json.dumps(load_outline(), ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    OUTPUT.write_text(TEMPLATE.replace("__BISAC_DATA__", data), encoding="utf-8")
    print(f"wrote {OUTPUT}")


if __name__ == "__main__":
    main()
