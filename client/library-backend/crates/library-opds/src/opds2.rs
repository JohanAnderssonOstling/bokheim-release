//! OPDS 2: JSON feeds. Read loosely, as feeds vary: `rel` may be a string or
//! a list, a contributor a string, an object or a list of either.
use crate::{choose_download, protected, push_facet, Facet, Feed, NavigationLink, Publication, Search};
use serde_json::Value;
use url::Url;

const ACQUISITION: &str = "http://opds-spec.org/acquisition";
const OPEN_ACCESS: &str = "http://opds-spec.org/acquisition/open-access";
const BUY: &str = "http://opds-spec.org/acquisition/buy";

pub(crate) fn parse(bytes: &[u8], base: &Url) -> Result<Feed, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|error| format!("not an OPDS 2 feed: {error}"))?;
    if !root.is_object() {
        return Err("not an OPDS 2 feed: not an object".into());
    }
    let mut feed = Feed { title: string(&root["metadata"]["title"]), navigation: navigation(&root["navigation"], base), publications: publications(&root["publications"], base), ..Feed::default() };
    feed.search = array(&root["links"]).find(|link| has_rel(link, "search")).and_then(|link| {
        let resolved = base.join(link["href"].as_str()?).ok()?;
        if link["type"].as_str().is_some_and(|kind| kind.contains("opensearchdescription")) {
            return Some(Search::Description(resolved));
        }
        // Resolving percent-encodes the template's braces; restore them.
        Some(Search::Template(resolved.as_str().replace("%7B", "{").replace("%7D", "}")))
    });
    feed.next = array(&root["links"]).find(|link| has_rel(link, "next")).and_then(|link| base.join(link["href"].as_str()?).ok());
    // A facet that is the current page is marked by a `self` link.
    for group in array(&root["facets"]) {
        for link in array(&group["links"]) {
            let Some(href) = link["href"].as_str().and_then(|href| base.join(href).ok()) else { continue };
            push_facet(&mut feed.facets, &string(&group["metadata"]["title"]), Facet { title: string(&link["title"]), href, active: has_rel(link, "self") });
        }
    }
    // A group with its own page is a link to it; its contents join this page.
    for group in array(&root["groups"]) {
        if let Some(href) = array(&group["links"]).find(|link| has_rel(link, "self")).and_then(|link| base.join(link["href"].as_str()?).ok()) {
            feed.navigation.push(NavigationLink { title: string(&group["metadata"]["title"]), href, count: None });
        }
        feed.navigation.extend(navigation(&group["navigation"], base));
        feed.publications.extend(publications(&group["publications"], base));
    }
    Ok(feed)
}

fn navigation(links: &Value, base: &Url) -> Vec<NavigationLink> {
    array(links).filter_map(|link| Some(NavigationLink { title: string(&link["title"]), href: base.join(link["href"].as_str()?).ok()?, count: number_of_items(link) })).collect()
}

fn publications(publications: &Value, base: &Url) -> Vec<Publication> {
    array(publications)
        .filter_map(|publication| {
            let metadata = &publication["metadata"];
            let open = array(&publication["links"]).filter(|link| has_rel(link, ACQUISITION) || has_rel(link, OPEN_ACCESS)).filter_map(|link| Some((base.join(link["href"].as_str()?).ok()?, link["type"].as_str().unwrap_or_default(), None)));
            let download = choose_download(open);
            let buy = array(&publication["links"]).find(|link| has_rel(link, BUY));
            // A book offered only with DRM, sold, lent or free, is not listed.
            if download.is_none()
                && array(&publication["links"]).filter(|link| rels(link).any(|rel| rel.starts_with(ACQUISITION))).any(|link| protected(link["type"].as_str()) || protected_acquisitions(&link["properties"]["indirectAcquisition"]))
            {
                return None;
            }
            Some(Publication {
                id: string(&metadata["identifier"]),
                title: string(&metadata["title"]),
                subtitle: metadata["subtitle"].as_str().map(str::to_owned),
                authors: contributors(&metadata["author"]),
                download,
                buy: buy.and_then(|link| base.join(link["href"].as_str()?).ok()),
                // The first image is the cover; OPDS 2 gives no order by size.
                cover: array(&publication["images"]).find_map(|image| base.join(image["href"].as_str()?).ok()),
                description: string(&metadata["description"]),
            })
        })
        .collect()
}

fn contributors(value: &Value) -> Vec<String> {
    match value {
        Value::String(name) => vec![name.clone()],
        Value::Object(_) => value["name"].as_str().map(|name| vec![name.to_owned()]).unwrap_or_default(),
        Value::Array(items) => items.iter().flat_map(contributors).collect(),
        _ => Vec::new(),
    }
}

fn rels(link: &Value) -> impl Iterator<Item = &str> {
    let rel = &link["rel"];
    rel.as_str().into_iter().chain(rel.as_array().into_iter().flatten().filter_map(Value::as_str))
}

fn has_rel(link: &Value, rel: &str) -> bool {
    rels(link).any(|value| value == rel)
}

fn number_of_items(link: &Value) -> Option<u32> {
    link["properties"]["numberOfItems"].as_u64().and_then(|count| u32::try_from(count).ok())
}

/// Whether any indirect acquisition requires a DRM license, including its children.
fn protected_acquisitions(acquisitions: &Value) -> bool {
    array(acquisitions).any(|acquisition| protected(acquisition["type"].as_str()) || protected_acquisitions(&acquisition["child"]))
}

fn array(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}
