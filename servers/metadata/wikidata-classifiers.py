"""Preserve classifier provenance; topic links produce explicitly inferred evidence.

The extraction keeps all external-ID and string claims, including unknown schemes.
Property definitions from the dump identify additional classification properties.
No classification schedule or topic-to-book equivalence is assumed.
"""
TOPIC_CLASS = 'Q95388829'
BOOK_CLASS = 'Q95388859'
# Book properties have string datatype; topic classifiers are external IDs.
KNOWN = {
 'P1036': ('ddc','topic'), 'P1149': ('lcc','topic'),
 'P1190': ('udc','topic'), 'P1150': ('rvk','topic'),
 'P5748': ('basisklassifikation','topic'), 'P8248': ('colon','topic'),
 'P12164': ('bisac','topic'),
 'P8359': ('ddc','book'), 'P8360': ('lcc','book'),
 'P8361': ('udc','book'), 'P8362': ('rvk','book'),
}

def values(statements):
    usable=[s for s in statements if s.get('mainsnak',{}).get('snaktype')=='value'
            and 'datavalue' in s.get('mainsnak',{})]
    active=[s for s in usable if s.get('rank')!='deprecated']
    preferred=[s for s in active if s.get('rank')=='preferred']
    return preferred or active or usable


def definitions(property_records):
    result=dict(KNOWN)
    for record in property_records:
        if record.get('type')!='property': continue
        types={s['mainsnak']['datavalue']['value'].get('id')
               for s in values(record.get('claims',{}).get('P31',[]))
               if isinstance(s['mainsnak']['datavalue']['value'],dict)}
        scope='book' if BOOK_CLASS in types else 'topic' if TOPIC_CLASS in types else None
        if scope:
            prop=record['id'];result[prop]=(result.get(prop,(prop,None))[0],scope)
    return result


def assigned(record, registry=None):
    registry=KNOWN if registry is None else registry
    result=[]
    for prop,statements in record.get('claims',{}).items():
        if prop not in registry:continue
        scheme,scope=registry[prop]
        for statement in values(statements):
            value=statement['mainsnak']['datavalue']['value']
            if not isinstance(value,str) or not value.strip():continue
            result.append(dict(scheme=scheme,property=prop,value=value,property_scope=scope,
                               source_entity=record['id'],method='wikidata:direct',
                               inferred=False,statement=statement))
    return result


def for_book(book, topics, registry=None):
    """Return direct and inferred evidence separately, using only explicit P921 links.

    `topics` is an ID -> record mapping from this same dump. Its identifiers and
    original statements remain in the result; callers must not flatten inferred
    results into exact-edition classifications or use them to merge identities.
    """
    direct=assigned(book,registry)
    inferred=[]
    for link in values(book.get('claims',{}).get('P921',[])):
        target=link['mainsnak']['datavalue']['value']
        if not isinstance(target,dict):continue
        topic_id=target.get('id');topic=topics.get(topic_id)
        if topic is None or topic.get('id')!=topic_id:continue
        for evidence in assigned(topic,registry):
            if evidence['property_scope']!='topic':continue
            inferred.append(dict(evidence,method='wikidata:topic:P921',inferred=True,
                                 book_entity=book['id'],topic_link=link))
    return dict(direct=direct,inferred=inferred)
