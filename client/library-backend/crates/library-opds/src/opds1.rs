//! OPDS 1.2: Atom feeds. An entry with acquisition links is a publication;
//! any other entry linking to a catalog page is navigation.
use crate::{bare_media_type, choose_download, protected, push_facet, Facet, Feed, NavigationLink, Publication, Search};
use roxmltree::Node;
use url::Url;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const OPDS: &str = "http://opds-spec.org/2010/catalog";
const THREAD: &str = "http://purl.org/syndication/thread/1.0";
const OPENSEARCH: &str = "http://a9.com/-/spec/opensearch/1.1/";
const ACQUISITION: &str = "http://opds-spec.org/acquisition";
const OPEN_ACCESS: &str = "http://opds-spec.org/acquisition/open-access";
const BUY: &str = "http://opds-spec.org/acquisition/buy";
const FACET: &str = "http://opds-spec.org/facet";
/// Cover links, full size first.
const COVERS: [&str; 2] = ["http://opds-spec.org/image", "http://opds-spec.org/image/thumbnail"];

pub(crate) fn parse(xml: &str, base: &Url) -> Result<Feed, String> {
    let document = roxmltree::Document::parse(xml).map_err(|error| format!("not an OPDS feed: {error}"))?;
    let root = document.root_element();
    if !is(&root, ATOM, "feed") {
        return Err("not an OPDS feed: no Atom feed element".into());
    }
    let mut feed = Feed { title: child_text(&root, "title"), ..Feed::default() };
    for link in children(&root, "link") {
        match link.attribute("rel") {
            Some("search") => feed.search = search(&link, base),
            Some("next") => feed.next = link.attribute("href").and_then(|href| base.join(href).ok()),
            Some(FACET) => {
                let Some(href) = link.attribute("href").and_then(|href| base.join(href).ok()) else { continue };
                let facet = Facet { title: link.attribute("title").unwrap_or_default().to_owned(), href, active: link.attribute((OPDS, "activeFacet")) == Some("true") };
                push_facet(&mut feed.facets, link.attribute((OPDS, "facetGroup")).unwrap_or("Filter"), facet);
            }
            _ => {}
        }
    }
    for entry in children(&root, "entry") {
        let links = children(&entry, "link").collect::<Vec<_>>();
        // Any acquisition makes a publication; only an open one is downloaded.
        if links.iter().any(|link| link.attribute("rel").is_some_and(|rel| rel.starts_with(ACQUISITION))) {
            let open = links
                .iter()
                .filter(|link| matches!(link.attribute("rel"), Some(ACQUISITION | OPEN_ACCESS)))
                .filter_map(|link| Some((base.join(link.attribute("href")?).ok()?, link.attribute("type").unwrap_or_default(), link.attribute("length").and_then(|length| length.parse().ok()))));
            let download = choose_download(open);
            let buy = links.iter().find(|link| link.attribute("rel") == Some(BUY));
            // A book offered only with DRM, sold, lent or free, is not listed.
            let offered_types = links
                .iter()
                .filter(|link| link.attribute("rel").is_some_and(|rel| rel.starts_with(ACQUISITION)))
                .flat_map(|link| link.descendants().filter(move |node| *node == *link || is(node, OPDS, "indirectAcquisition")))
                .filter_map(|node| node.attribute("type"));
            if download.is_none() && protected(offered_types) {
                continue;
            }
            feed.publications.push(Publication {
                id: child_text(&entry, "id"),
                title: child_text(&entry, "title"),
                subtitle: None,
                authors: children(&entry, "author").map(|author| child_text(&author, "name")).filter(|name| !name.is_empty()).collect(),
                download,
                buy: buy.and_then(|link| base.join(link.attribute("href")?).ok()),
                cover: COVERS.into_iter().find_map(|rel| links.iter().find(|link| link.attribute("rel") == Some(rel))?.attribute("href").and_then(|href| base.join(href).ok())),
                description: description(&entry, xml),
            });
            continue;
        }
        let Some(page) = links.iter().find(|link| link.attribute("type").is_some_and(|kind| kind.contains("opds-catalog") || bare_media_type(kind) == "application/atom+xml")) else { continue };
        let Some(href) = page.attribute("href").and_then(|href| base.join(href).ok()) else { continue };
        feed.navigation.push(NavigationLink { title: child_text(&entry, "title"), href, count: page.attribute((THREAD, "count")).and_then(|count| count.parse().ok()) });
    }
    Ok(feed)
}

/// The URL template from an OpenSearch description: the one returning a feed.
pub fn opensearch_template(xml: &str) -> Result<String, String> {
    let document = roxmltree::Document::parse(xml).map_err(|error| format!("not an OpenSearch description: {error}"))?;
    let urls = document.root_element().children().filter(|node| is(node, OPENSEARCH, "Url")).collect::<Vec<_>>();
    let feed = |url: &&Node| url.attribute("type").is_some_and(|kind| kind.contains("atom+xml") || kind.contains("opds"));
    urls.iter().find(feed).and_then(|url| url.attribute("template")).map(str::to_owned).ok_or_else(|| "the OpenSearch description has no feed template".into())
}

fn search(link: &Node, base: &Url) -> Option<Search> {
    let resolved = base.join(link.attribute("href")?).ok()?;
    if link.attribute("type").is_some_and(|kind| kind.contains("opensearchdescription")) {
        return Some(Search::Description(resolved));
    }
    // Resolving percent-encodes the braces of the placeholder; restore them.
    Some(Search::Template(resolved.as_str().replace("%7BsearchTerms%7D", "{searchTerms}")))
}

/// An entry's description as HTML: its `content`, else its `summary`, read
/// by the type the feed gives it. XHTML is kept as markup; plain text is
/// escaped.
fn description(entry: &Node, xml: &str) -> String {
    let Some(node) = children(entry, "content").next().or_else(|| children(entry, "summary").next()) else { return String::new() };
    match node.attribute("type") {
        Some("xhtml") => match (node.first_child(), node.last_child()) {
            (Some(first), Some(last)) => xml[first.range().start..last.range().end].trim().to_owned(),
            _ => String::new(),
        },
        Some("html") => node.text().unwrap_or_default().trim().to_owned(),
        _ => node.text().unwrap_or_default().trim().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"),
    }
}

fn child_text(node: &Node, name: &'static str) -> String {
    children(node, name).next().and_then(|child| child.text()).map(|text| text.trim().to_owned()).unwrap_or_default()
}

fn children<'a, 'input>(node: &Node<'a, 'input>, name: &'static str) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(move |child| is(child, ATOM, name))
}

fn is(node: &Node, namespace: &str, name: &str) -> bool {
    node.tag_name().namespace() == Some(namespace) && node.tag_name().name() == name
}
