// SiteOne Crawler - FoundUrls collection
// (c) Jan Reges <jan.reges@siteone.cz>

use std::collections::HashMap;

use md5::{Digest, Md5};
use once_cell::sync::Lazy;
use regex::Regex;

use super::found_url::{FoundUrl, UrlSource};

/// Regex for detecting a URL with a scheme (RFC 3986: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`
/// followed by `:`), e.g. mailto:, javascript:, data:, tel:+1…, android-app://, web+app:
static SCHEME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.\-]*:").unwrap());

/// Regex for the http(s) schemes, in any letter case
static HTTP_SCHEME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^https?:").unwrap());

/// Collection of found URLs, deduplicated by MD5 hash of URL
#[derive(Debug, Clone)]
pub struct FoundUrls {
    found_urls: HashMap<String, FoundUrl>,
}

impl FoundUrls {
    pub fn new() -> Self {
        Self {
            found_urls: HashMap::new(),
        }
    }

    /// Add a found URL, deduplicated by MD5 hash
    pub fn add_url(&mut self, found_url: FoundUrl) {
        let key = md5_hex(&found_url.url);
        self.found_urls.entry(key).or_insert(found_url);
    }

    /// Add URLs from a text array, filtering out invalid ones
    pub fn add_urls_from_text_array(&mut self, urls: &[&str], source_url: &str, source: UrlSource) {
        for url in urls {
            if is_url_valid_for_crawling(url) {
                self.add_url(FoundUrl::new(url, source_url, source));
            }
        }
    }

    /// Add one URL with the byte offset of its text in the extracted content (leading whitespace
    /// of the value skipped) and its link text, filtering out invalid ones like
    /// `add_urls_from_text_array`, but keeping a mistyped http(s) scheme (not in script text) for
    /// the crawler to report. The first occurrence of a URL keeps its details.
    pub fn add_url_with_details(
        &mut self,
        url: &str,
        offset: usize,
        text: Option<Box<str>>,
        source_url: &str,
        source: UrlSource,
    ) {
        if is_url_valid_for_crawling(url) || (source != UrlSource::InlineScriptSrc && has_mistyped_http_scheme(url)) {
            let offset = offset + (url.len() - url.trim_ascii_start().len());
            self.add_url(FoundUrl::new(url, source_url, source).with_details(Some(offset), text));
        }
    }

    /// Replace every known offset by `map(offset)`, e.g. to refer to the content before something
    /// was cut out of it.
    pub fn map_offsets(&mut self, map: impl Fn(usize) -> usize) {
        for found_url in self.found_urls.values_mut() {
            found_url.offset = found_url.offset.map(&map);
        }
    }

    /// Get all found URLs
    pub fn get_urls(&self) -> &HashMap<String, FoundUrl> {
        &self.found_urls
    }

    /// Get count of found URLs
    pub fn get_count(&self) -> usize {
        self.found_urls.len()
    }
}

impl Default for FoundUrls {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute MD5 hex hash of a string
fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    crate::utils::to_lower_hex(hasher.finalize())
}

/// Check if URL is valid for crawling. Ignored are:
/// - anchor #fragment links
/// - data:, mailto:, javascript: and other non-http(s) links
/// - file:// links
fn is_url_valid_for_crawling(url: &str) -> bool {
    let url = url.trim_ascii();
    if url.starts_with('#') {
        return false;
    }
    if SCHEME_RE.is_match(url) && !HTTP_SCHEME_RE.is_match(url) {
        return false;
    }
    if url.to_lowercase().starts_with("file://") {
        return false;
    }
    true
}

/// Whether the scheme of `url` is a typo of `http` or `https`: neither of them, but one edit away
/// (a character deleted, inserted or replaced, or two adjacent characters swapped), e.g. `htp:`,
/// `htps:`, `httsp:`. A visitor who follows such a link gets an error.
pub fn has_mistyped_http_scheme(url: &str) -> bool {
    let Some(scheme) = SCHEME_RE.find(url.trim_ascii()) else {
        return false;
    };
    let scheme = scheme.as_str().trim_end_matches(':').to_ascii_lowercase();
    scheme != "http" && scheme != "https" && (is_one_edit_away(&scheme, "http") || is_one_edit_away(&scheme, "https"))
}

/// Whether `a` and `b` differ by exactly one deleted, inserted or replaced character, or by two
/// swapped adjacent characters.
fn is_one_edit_away(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    match (a.len(), b.len()) {
        (0, 1) | (1, 0) | (1, 1) => true,
        (2, 2) => a[0] == b[1] && a[1] == b[0],
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_by_md5() {
        let mut urls = FoundUrls::new();
        urls.add_url(FoundUrl::new("/page", "https://example.com/", UrlSource::AHref));
        urls.add_url(FoundUrl::new("/page", "https://example.com/other", UrlSource::AHref));
        assert_eq!(urls.get_count(), 1);
    }

    #[test]
    fn test_add_urls_from_text_array() {
        let mut urls = FoundUrls::new();
        urls.add_urls_from_text_array(
            &["/page1", "/page2", "#fragment", "mailto:test@test.com", "/page1"],
            "https://example.com/",
            UrlSource::AHref,
        );
        assert_eq!(urls.get_count(), 2);
    }

    #[test]
    fn test_is_url_valid_for_crawling() {
        assert!(is_url_valid_for_crawling("/page"));
        assert!(is_url_valid_for_crawling("https://example.com"));
        assert!(!is_url_valid_for_crawling("#fragment"));
        assert!(!is_url_valid_for_crawling("mailto:test@test.com"));
        assert!(!is_url_valid_for_crawling("javascript:void(0)"));
        assert!(!is_url_valid_for_crawling("data:text/html,test"));
        assert!(!is_url_valid_for_crawling("file:///etc/passwd"));
    }

    #[test]
    fn only_ascii_whitespace_around_a_found_url_is_not_part_of_it() {
        let mut urls = FoundUrls::new();
        urls.add_url_with_details(" \u{a0}/x", 10, None, "https://example.com/", UrlSource::AHref);
        let found = urls.get_urls().values().next().expect("the URL is kept");
        assert_eq!(found.url, "\u{a0}/x");
        assert_eq!(found.offset, Some(11), "the URL text starts at its non-breaking space");
        assert!(is_url_valid_for_crawling("\u{a0}#x"), "a relative path, not a fragment");
    }

    #[test]
    fn a_scheme_one_edit_away_from_http_or_https_is_mistyped() {
        for url in [
            "htp://example.com/",
            "ttp://example.com/",
            "htt://example.com/",
            "hhtp://example.com/",
            "htpp://example.com/",
            "httpp://example.com/",
            "htpt://example.com/",
            "htps://example.com/",
            "ttps://example.com/",
            "httsp://example.com/",
            "httpss://example.com/",
            "htpps://example.com/",
            "HTP://example.com/",
            "Htps:example.com",
        ] {
            assert!(has_mistyped_http_scheme(url), "{url}");
        }
        for url in [
            "http://example.com/",
            "https://example.com/",
            "HTTPS://example.com/",
            "ftp://example.com/",
            "sftp://example.com/",
            "mailto:a@example.com",
            "tel:+15551234567",
            "data:text/plain,x",
            "hxxp://example.com/",
            "ws://example.com/",
            "zoommtg://example.com/join",
            "/htp://example.com/",
            "htp//example.com/",
            "page.html",
        ] {
            assert!(!has_mistyped_http_scheme(url), "{url}");
        }
    }

    #[test]
    fn a_mistyped_scheme_is_found_in_attributes_but_not_in_script_text() {
        let mut urls = FoundUrls::new();
        urls.add_url_with_details("htp://example.com/a", 0, None, "https://example.com/", UrlSource::AHref);
        urls.add_url_with_details(
            "htps://example.com/b",
            0,
            None,
            "https://example.com/",
            UrlSource::InlineScriptSrc,
        );
        urls.add_urls_from_text_array(&["htps://example.com/c"], "https://example.com/", UrlSource::JsUrl);
        let found: Vec<&str> = urls.get_urls().values().map(|u| u.url.as_str()).collect();
        assert_eq!(found, ["htp://example.com/a"]);
    }

    #[test]
    fn non_http_schemes_follow_rfc_3986_scheme_syntax() {
        // scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ), whatever follows the colon
        for url in [
            "android-app://com.example/https/example.com/x",
            "ms-windows-store://pdp/?productid=X",
            "fb-messenger://share?link=x",
            "web+app:thing",
            "x.y-z+1:anything",
            "tel:+15551234567",
            "whatsapp://send?text=hi",
            "sms:?body=x",
            "MAILTO:a@b.c",
            // An RFC 3986 scheme, as browsers read them too: never a relative path (not crawled today).
            "localhost:8080/x",
            "C:/x",
        ] {
            assert!(!is_url_valid_for_crawling(url), "{url}");
        }
        for url in [
            "http://example.com/x",
            "https://example.com/x",
            "HTTPS://EXAMPLE.COM/x",
            "Http://example.com/x",
            "//cdn.example.com/x",
            "/a:b",
            "./a:b",
            "a/b:c",
            "?q=a:b",
            "page.html?x=a:b",
            "1:x",
        ] {
            assert!(is_url_valid_for_crawling(url), "{url}");
        }
    }
}
