#!/usr/bin/env python3
import re, sqlite3, sys
DB=sys.argv[1] if len(sys.argv)>1 else '/home/johan/Hem/Programmering/RustroverProjects/Html-Prototype/shared/subject-projection/data/unified-taxonomy-v2.sqlite3'
pat=re.compile(r'^\s*(?:to |through )?\d{1,4}(?:\s*(?:-|–|/)\s*(?:\d{1,4})?)?\.?\s*$',re.I)
names={
'Benelux Countries':{
'Through 1400':'Medieval Benelux, to 1400','1401-1600':'Burgundian & Habsburg Benelux, 1401–1600','1601-1700':'Dutch Golden Age, 1601–1700','1701-1800':'Eighteenth-Century Benelux, 1701–1800','1801-1900':'Nineteenth-Century Benelux, 1801–1900','1901-1950':'World Wars in Benelux, 1901–1950','1951-':'Postwar Benelux, 1951–Present'},
'Poland':{'To 1795':'Poland to the Partitions, to 1795','To 1572':'Piast & Jagiellonian Poland, to 1572'},
'France':{
'687-1514':'Medieval France, 687–1514','1515-1588':'Valois France, 1515–1588','1589-1714':'Bourbon France, 1589–1714','1715-1788':'Ancien Régime France, 1715–1788','1789-1815':'Revolutionary & Napoleonic France, 1789–1815','1816-1870':'Restoration & Imperial France, 1816–1870','1871-1945':'Republic & World Wars France, 1871–1945','1946-1974':'Postwar France, 1946–1974','1975-':'Contemporary France, 1975–Present'},
'Germany':{
'911-1518':'Medieval Germany, 911–1518','1519-1648':'Reformation Germany, 1519–1648','1649-1744':'Post-Westphalian Germany, 1649–1744','1745-1788':'Enlightenment Germany, 1745–1788','1789-1815':'Napoleonic Germany, 1789–1815','1816-1829':'Restoration Germany, 1816–1829','1830-1870':'Unification Era Germany, 1830–1870','1871-1918':'German Empire, 1871–1918','1919-1945':'Weimar & Nazi Germany, 1919–1945','1946-':'Postwar Germany, 1946–Present'},
'Bulgaria':{
'681-893':'Early First Bulgarian Empire, 681–893','893-1018':'Late First Bulgarian Empire, 893–1018','1878-1944':'Principality & Kingdom of Bulgaria, 1878–1944','1944-1990':'Communist Bulgaria, 1944–1990','1990-':'Republic of Bulgaria, 1990–Present'},
'Romania':{
'1526-1918':'Habsburg Transylvania, 1526–1918','1822-1866':'National Revival Romania, 1822–1866','1866-1876':'United Principalities of Romania, 1866–1876','1876-1881':'Independent Romania, 1876–1881','1918-':'Modern Romania, 1918–Present','1918-1944':'Greater Romania, 1918–1944','1944-1989':'Communist Romania, 1944–1989','1989-':'Post-Communist Romania, 1989–Present'},
'Denmark':{
'750-1042':'Viking Denmark, 750–1042','1042-1241':'High Medieval Denmark, 1042–1241','1241-1387':'Late Medieval Denmark, 1241–1387','1387-1523':'Kalmar Union Denmark, 1387–1523','1523-1670':'Early Modern Denmark, 1523–1670','1670-1808':'Absolute Monarchy Denmark, 1670–1808'},
'Finland':{
'Through 1800':'Early Finland, to 1800','1249-1362':'Early Swedish Finland, 1249–1362','1362-1397':'Late Medieval Finland, 1362–1397','1523-1617':'Vasa Finland, 1523–1617','1617-1721':'Great Power Era Finland, 1617–1721','1721-1809':'Late Swedish Finland, 1721–1809','1800-1917':'Russian Finland, 1800–1917','1917-':'Independent Finland, 1917–Present','1939-1945':'World War II Finland, 1939–1945'},
'Iceland':{
'874-1262':'Commonwealth Iceland, 874–1262','1262-1540':'Norwegian Iceland, 1262–1540','1540-1800':'Danish Iceland, 1540–1800','1801-1918':'National Awakening Iceland, 1801–1918','1918-':'Modern Iceland, 1918–Present'},
'Norway':{
'872-1035':'Viking Kingdom of Norway, 872–1035','1035-1319':'Medieval Norway, 1035–1319','1319-1387':'Late Medieval Norway, 1319–1387','1387-1814':'Danish Norway, 1387–1814'},
'Switzerland':{
'Through 1500':'Old Swiss Confederacy, to 1500','1501-1800':'Early Modern Switzerland, 1501–1800','1801-1900':'Federal Transformation Switzerland, 1801–1900','1901-1950':'World Wars Switzerland, 1901–1950','1951-1980':'Postwar Switzerland, 1951–1980','1981':'Contemporary Switzerland, from 1981'},
}
c=sqlite3.connect(DB); c.row_factory=sqlite3.Row
q='''WITH RECURSIVE e(id,path) AS (SELECT concept_id,preferred_label FROM concept WHERE preferred_label='Europe' UNION ALL SELECT x.concept_id,e.path||' > '||x.preferred_label FROM e JOIN concept_parent p ON p.parent_concept_id=e.id JOIN concept x ON x.concept_id=p.concept_id) SELECT e.id,e.path,x.preferred_label FROM e JOIN concept x ON x.concept_id=e.id WHERE EXISTS(SELECT 1 FROM source_selector s WHERE s.concept_id=e.id)'''
changes=[]
for r in c.execute(q):
 if not pat.match(r['preferred_label']) or 'History of the Greco-Roman world' in r['path']: continue
 owner=next((k for k in names if f'> {k} >' in r['path'] or r['path'].endswith('> '+k)),None)
 if owner and r['preferred_label'] in names[owner]: new=names[owner][r['preferred_label']]
 else:
  parts=r['path'].split(' > '); owner=parts[-2]
  old=r['preferred_label'].replace('-', '–')
  new=f'{owner}, {old}'
 changes.append((new,r['id'],r['preferred_label'],r['path']))
assert len(changes)==71,len(changes)
with c:
 for new,cid,old,path in changes: c.execute('update concept set preferred_label=? where concept_id=?',(new,cid))
print('renamed',len(changes))
for new,cid,old,path in changes: print(old,'=>',new)
