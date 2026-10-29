//! Client-owned discovery for transport-independent remote EPUB warming.

use std::{collections::BTreeSet, io, sync::Arc};

use super::{
    EpubPackage,
    archive::{ArchiveMetadata, ArchiveReader},
};
use crate::{
    BookReader, resolve_resource_href,
    source::{ReaderFactory, Source},
};

pub struct EpubOpeningPlan {
    metadata: Arc<ArchiveMetadata>,
}

impl EpubOpeningPlan {
    /// Only parses the ZIP directory; a seeded tail usually satisfies this read.
    pub async fn read<F, R>(factory: F) -> io::Result<Self>
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: BookReader + 'static,
    {
        let reader = ArchiveReader::open(Source::reopenable(ReaderFactory::new(factory))).await?;
        Ok(Self { metadata: reader.metadata })
    }

    /// Speculative first pass, verified against the package after fetching.
    /// Unknown filenames never remove resources from eager preparation.
    pub fn initial_ranges(&self) -> io::Result<Vec<(u64, usize)>> {
        let names = self
            .metadata
            .names()
            .iter()
            .filter(|name| {
                let name = name.to_ascii_lowercase();
                name == "meta-inf/container.xml" || [".html", ".htm", ".xhtml", ".css", ".opf", ".ncx"].iter().any(|extension| name.ends_with(extension))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.metadata.record_ranges(&names)
    }

    /// The OPF is authoritative, including HTML/CSS with unusual extensions,
    /// auxiliary HTML, navigation and the same legacy spine fallback as reading.
    pub async fn required_ranges<F, R>(&self, factory: F) -> io::Result<Vec<(u64, usize)>>
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: BookReader + 'static,
    {
        let mut reader = ArchiveReader::with_metadata(self.metadata.clone(), Source::reopenable(ReaderFactory::new(factory)));
        let package = EpubPackage::open(&mut reader).await?;
        let manifest = package.get_or_parse_manifest();
        let mut names = BTreeSet::new();
        for name in ["META-INF/container.xml", package.opf_path.as_str()] {
            if let Some(path) = self.metadata.resolve(name) {
                names.insert(path);
            }
        }
        names.extend(package.document_uris()?);
        for item in &manifest.items {
            if item.media_type.as_deref().is_some_and(|kind| ["application/xhtml+xml", "text/html", "text/css"].iter().any(|expected| kind.eq_ignore_ascii_case(expected))) {
                let name = resolve_resource_href(&package.opf_path, &item.href);
                if super::is_container_resource(&name)
                    && let Some(path) = self.metadata.resolve(&name)
                {
                    names.insert(path);
                }
            }
        }
        for href in [&manifest.nav_href, &manifest.ncx_href].into_iter().flatten() {
            let name = resolve_resource_href(&package.opf_path, href);
            // Navigation is optional and has a fallback in the normal reader.
            if self.metadata.contains(&name) {
                names.insert(name);
            }
        }
        self.metadata.record_ranges(&names.into_iter().collect::<Vec<_>>())
    }
}
