//! The parsers against saved feeds: Project Gutenberg (OPDS 1.2), Readium's
//! test catalog (OPDS 2) and a hand-made acquisition feed.
use library_opds::{opensearch_template, parse, Search, EPUB};
use url::Url;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn url(value: &str) -> Url {
    Url::parse(value).unwrap()
}

#[test]
fn gutenberg_root_is_navigation_with_opensearch() {
    let feed = parse(&fixture("gutenberg-root.xml"), Some("application/atom+xml;profile=opds-catalog"), &url("https://www.gutenberg.org/ebooks.opds/")).unwrap();
    assert_eq!(feed.title, "Project Gutenberg");
    assert_eq!(feed.navigation.iter().map(|link| link.title.as_str()).collect::<Vec<_>>(), ["Popular", "Latest", "Random"]);
    assert_eq!(feed.navigation[0].href, url("https://www.gutenberg.org/ebooks/search.opds/?sort_order=downloads"));
    assert!(feed.publications.is_empty());
    assert_eq!(feed.search, Some(Search::Description(url("https://www.gutenberg.org/catalog/osd-books.xml"))));
}

#[test]
fn manybooks_root_lists_categories_with_plain_atom_links() {
    let feed = parse(&fixture("manybooks-root.xml"), Some("application/xml"), &url("https://manybooks.net/opds")).unwrap();
    assert_eq!(feed.navigation.iter().map(|link| link.title.as_str()).collect::<Vec<_>>(), ["New Titles", "Authors", "Titles", "Genres"]);
    assert_eq!(feed.navigation[0].href, url("https://manybooks.net/opds/new_titles"));
    let Some(Search::Template(template)) = feed.search else { panic!("the catalog has no search") };
    assert_eq!(Search::url(&template, "pride & prejudice").unwrap(), url("https://manybooks.net/opds/search?q=pride+%26+prejudice"));
}

#[test]
fn gutenberg_search_results_link_each_book_to_its_page() {
    let feed = parse(&fixture("gutenberg-search.xml"), None, &url("https://www.gutenberg.org/ebooks/search.opds/?sort_order=downloads")).unwrap();
    // Each book is a link to its own page, which holds the downloads.
    assert_eq!(feed.navigation.len(), 25);
    assert!(feed.navigation.iter().any(|link| link.title == "Pride and Prejudice" && link.href == url("https://www.gutenberg.org/ebooks/1342.opds")));
    assert_eq!(feed.next, Some(url("https://www.gutenberg.org/ebooks/search.opds/?sort_order=downloads&start_index=26")));
}

#[test]
fn gutenberg_book_page_offers_the_illustrated_edition() {
    let feed = parse(&fixture("gutenberg-book.xml"), Some("application/atom+xml"), &url("https://www.gutenberg.org/ebooks/1342.opds")).unwrap();
    // The editions without and with images are one book; the larger,
    // illustrated one is kept.
    assert_eq!(feed.publications.iter().map(|publication| publication.id.as_str()).collect::<Vec<_>>(), ["urn:gutenberg:1342:3"]);
    let first = &feed.publications[0];
    assert_eq!(first.title, "Pride and Prejudice");
    assert_eq!(first.authors, ["Austen, Jane"]);
    let download = first.download.as_ref().unwrap();
    assert_eq!(download.media_type, EPUB);
    assert_eq!(download.href, url("https://www.gutenberg.org/ebooks/1342.epub3.images"));
    // The full-size image over the thumbnail.
    assert_eq!(first.cover, Some(url("https://www.gutenberg.org/cache/epub/1342/pg1342.cover.medium.jpg")));
}

#[test]
fn opensearch_description_gives_the_feed_template() {
    let template = opensearch_template(std::str::from_utf8(&fixture("gutenberg-opensearch.xml")).unwrap()).unwrap();
    assert_eq!(template, "http://m.gutenberg.org/ebooks/search.opds/?query={searchTerms}");
    assert_eq!(Search::url(&template, "pride & prejudice").unwrap(), url("http://m.gutenberg.org/ebooks/search.opds/?query=pride+%26+prejudice"));
}

#[test]
fn acquisition_feed_offers_open_downloads_facets_paging_and_search() {
    let feed = parse(&fixture("acquisition.xml"), Some("application/atom+xml"), &url("http://calibre.local:8080/opds")).unwrap();
    // The DRM-only entry is not listed.
    assert_eq!(feed.publications.iter().map(|publication| publication.title.as_str()).collect::<Vec<_>>(), ["The Hobbit", "For Sale"]);
    // XHTML content is kept as markup.
    assert!(feed.publications[0].description.starts_with("<div") && feed.publications[0].description.contains("<p>In a hole in the ground there lived a hobbit.</p>"));
    // EPUB is chosen over the PDF the entry also offers.
    assert_eq!(feed.publications[0].download.as_ref().unwrap().href, url("http://calibre.local:8080/get/epub/12/Calibre"));
    // The buy-only entry offers nothing to download, but its store page.
    assert!(feed.publications[1].download.is_none());
    assert_eq!(feed.publications[1].buy, Some(url("https://shop.example.org/book/14")));
    assert_eq!(feed.navigation.iter().map(|link| link.title.as_str()).collect::<Vec<_>>(), ["Authors"]);
    assert_eq!(feed.next, Some(url("http://calibre.local:8080/opds/navcatalog/4e6577657374?library_id=Calibre&offset=2")));
    assert_eq!(feed.facets.len(), 1);
    assert_eq!(feed.facets[0].title, "Sort by");
    assert_eq!(feed.facets[0].facets.iter().map(|facet| (facet.title.as_str(), facet.active)).collect::<Vec<_>>(), [("Newest", true), ("Title", false)]);
    let Some(Search::Template(template)) = &feed.search else { panic!("expected a search template, got {:?}", feed.search) };
    assert_eq!(Search::url(template, "hobbit").unwrap(), url("http://calibre.local:8080/opds/search/hobbit?library_id=Calibre"));
}

#[test]
fn opds2_home_flattens_groups_into_the_page() {
    let feed = parse(&fixture("opds2-home.json"), Some("application/opds+json"), &url("https://test.opds.io/2.0/home.json")).unwrap();
    assert_eq!(feed.title, "OPDS 2.0 Test Catalog");
    assert_eq!(feed.navigation[0].title, "Explore (Navigation Feed)");
    // 8 links of its own, "French Classics" (a group with its own page) and
    // the 2 links of "More Navigation".
    assert_eq!(feed.navigation.len(), 11);
    assert!(feed.navigation.iter().any(|link| link.title == "French Classics" && link.href == url("https://test.opds.io/2.0/publications.json")));
    // The groups repeat two root entries; each book still has one card.
    assert_eq!(
        feed.publications.iter().map(|publication| publication.title.as_str()).collect::<Vec<_>>(),
        ["Moby-Dick", "Le Mystère de la chambre jaune", "Voyage au centre de la Terre", "Le Rouge et le Noir", "Madame Bovary", "Du côté de chez Swann", "Romeo and Juliet", "Jane Eyre"]
    );
}

#[test]
fn opds2_publication_reads_contributors_and_open_download() {
    let feed = parse(&fixture("opds2-publications.json"), None, &url("https://test.opds.io/2.0/publications.json")).unwrap();
    assert_eq!(feed.publications.len(), 14);
    let verne = &feed.publications[0];
    assert_eq!(verne.title, "Voyage au centre de la Terre");
    assert_eq!(verne.subtitle.as_deref(), Some("Un super sous-titre"));
    assert_eq!(verne.authors, ["Jules Verne"]);
    assert!(verne.description.starts_with("Le professeur Lidenbrock"));
    assert_eq!(verne.download.as_ref().unwrap().href, url("https://test.opds.io/assets/centredelaterre/file.epub"));
}

#[test]
fn opds2_filters_drm_acquisitions_but_retains_open_and_unprotected_offers() {
    use serde_json::{json, Value};

    let license = "Application/Vnd.Readium.Lcp.License.V1.0+Json; charset=utf-8";
    let protected = json!({"type": "application/epub+zip", "child": [
        {"type": "application/pdf"},
        {"child": [{"type": license}]}
    ]});
    let buy = |properties: Value| {
        json!({
            "rel": ["alternate", "http://opds-spec.org/acquisition/buy"],
            "href": "shop", "type": "text/html", "properties": properties
        })
    };
    let cases = [
        ("Direct Adobe license", json!([{"rel": "http://opds-spec.org/acquisition/borrow", "href": "loan", "type": "application/vnd.adobe.adept+xml"}]), false),
        ("Nested LCP license", json!([buy(json!({"indirectAcquisition": [protected]}))]), false),
        ("Later protected sibling", json!([buy(json!({"indirectAcquisition": [{"type": "application/pdf"}, {"type": license}]}))]), false),
        ("Unprotected offer", json!([buy(json!({"indirectAcquisition": [{"type": "application/pdf", "child": [{"type": "application/epub+zip"}]}]}))]), true),
        ("Missing optional types", json!([buy(json!({"indirectAcquisition": [null, {}, {"type": 42, "child": "not an array"}]}))]), true),
        ("Non-acquisition license", json!([{"rel": "alternate", "href": "license", "type": license}]), true),
        (
            "Open alternative",
            json!([
                buy(json!({"indirectAcquisition": [protected]})),
                {"rel": "http://opds-spec.org/acquisition/open-access", "href": "book.epub", "type": "application/epub+zip"}
            ]),
            true,
        ),
    ];
    for (title, links, listed) in cases {
        let bytes = serde_json::to_vec(&json!({"publications": [{"metadata": {"title": title}, "links": links}]})).unwrap();
        let feed = parse(&bytes, Some("application/opds+json"), &url("https://catalog.example/feed")).unwrap();
        assert_eq!(feed.publications.len(), usize::from(listed), "{title}");
        if title == "Open alternative" {
            assert_eq!(feed.publications[0].download.as_ref().unwrap().href, url("https://catalog.example/book.epub"));
            assert_eq!(feed.publications[0].buy, Some(url("https://catalog.example/shop")));
        }
    }
}

fn acquisition_publication(version: u8, links: &[(&str, &str)]) -> library_opds::Publication {
    let bytes = if version == 1 {
        let links = links.iter().map(|(href, kind)| format!(r#"<link rel="http://opds-spec.org/acquisition" href="{href}" type="{kind}" length="42"/>"#)).collect::<String>();
        format!(r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>Book</title>{links}</entry></feed>"#).into_bytes()
    } else {
        use serde_json::json;
        let links = links.iter().map(|(href, kind)| json!({"rel": "http://opds-spec.org/acquisition/open-access", "href": href, "type": kind})).collect::<Vec<_>>();
        serde_json::to_vec(&json!({"publications": [{"metadata": {"title": "Book"}, "links": links}]})).unwrap()
    };
    parse(&bytes, None, &url("https://catalog.example/feed")).unwrap().publications.remove(0)
}

#[test]
fn acquisition_downloads_prefer_supported_formats_in_both_versions() {
    use library_opds::{CBZ, CBZ_LEGACY, FB2, FB2_ZIP, PDF};
    let formats = [EPUB, FB2, FB2_ZIP, PDF, CBZ, CBZ_LEGACY];
    for first in 0..formats.len() {
        let links = formats[first..].iter().rev().map(|kind| ("download", *kind)).collect::<Vec<_>>();
        for version in [1, 2] {
            let download = acquisition_publication(version, &links).download.unwrap();
            assert_eq!(download.media_type, formats[first], "OPDS {version}");
            assert_eq!(download.length, if version == 1 { Some(42) } else { None });
        }
    }
}

#[test]
fn acquisition_downloads_keep_the_first_link_for_a_matching_format() {
    for version in [1, 2] {
        let publication = acquisition_publication(version, &[("first.epub", " Application/EPUB+ZIP ; charset=utf-8"), ("second.epub", EPUB)]);
        let download = publication.download.unwrap();
        assert_eq!(download.media_type, EPUB);
        assert_eq!(download.href, url("https://catalog.example/first.epub"));
    }
}

#[test]
fn acquisition_downloads_skip_unsupported_types_and_invalid_urls() {
    for version in [1, 2] {
        let unsupported = acquisition_publication(version, &[("archive.zip", "application/zip"), ("missing", ""), ("partial", "application/epub+zip-extra")]);
        assert_eq!(unsupported.download, None);
        let fallback = acquisition_publication(version, &[("http://[", EPUB), ("book.pdf", "application/pdf")]);
        assert_eq!(fallback.download.unwrap().href, url("https://catalog.example/book.pdf"));
    }
}
