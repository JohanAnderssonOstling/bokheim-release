//! Artwork for an already selected recording, fetched through bounded provider requests.
use super::*;

fn artwork_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("m.media-amazon.com" | "images-na.ssl-images-amazon.com" | "images-eu.ssl-images-amazon.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err("Unsupported audiobook artwork origin".into());
    }
    Ok(url)
}

impl AudibleClient {
    pub(crate) async fn cover(&self, region: &str, asin: &str) -> Result<Option<Vec<u8>>, String> {
        if !parsing::valid_asin(asin) {
            return Err("Invalid ASIN".into());
        }
        let (_, api, _) = self.origins.iter().find(|(r, _, _)| r == region).ok_or("Invalid region")?;
        let _permit = self.lookups.try_acquire().map_err(|_| "Artwork capacity busy")?;
        let book = self.json(&self.audnexus_origin, &format!("/books/{asin}"), &[("region", region)]).await;
        if let Ok(book) = &book {
            if book.get("asin").and_then(Value::as_str) == Some(asin) && book.get("region").and_then(Value::as_str) == Some(region) {
                if let Some(image) = book.get("image").and_then(Value::as_str) {
                    if let Ok(url) = artwork_url(image) {
                        if let Ok(bytes) = self.get(url).await {
                            return Ok(Some(bytes.as_ref().clone()));
                        }
                    }
                }
            }
        }
        // Audible remains available when Audnexus lacks artwork or is unavailable.
        let value = self.json(api, &format!("/1.0/catalog/products/{asin}"), &[("response_groups", "product_desc,product_attrs"), ("image_sizes", "500")]).await?;
        let product = value.get("product").ok_or("Audible product missing")?;
        if product.get("asin").and_then(Value::as_str) != Some(asin) {
            return Err("Audible returned a different ASIN".into());
        }
        let images = product.get("product_images").and_then(Value::as_object);
        let image = images.and_then(|images| images.iter().filter_map(|(size, url)| Some((size.parse::<u32>().ok()?, url.as_str()?))).max_by_key(|(size, _)| *size));
        match image {
            Some((_, image)) => Ok(Some(self.get(artwork_url(image)?).await?.as_ref().clone())),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artwork_proxy_only_accepts_trusted_https_image_hosts() {
        assert!(artwork_url("https://m.media-amazon.com/images/I/cover.jpg").is_ok());
        for url in ["http://m.media-amazon.com/a", "https://m.media-amazon.com.evil.test/a", "https://127.0.0.1/a", "https://user@m.media-amazon.com/a", "https://m.media-amazon.com:444/a"] {
            assert!(artwork_url(url).is_err(), "{url}");
        }
    }
}
