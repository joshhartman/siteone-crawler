// SiteOne Crawler - CssProcessor
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Extracts URLs from CSS url() and @import, and converts for offline use.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::content_processor::base_processor::{ProcessorConfig, StoredUrlFn, convert_url_to_relative, is_relevant};
use crate::content_processor::content_processor::ContentProcessor;
use crate::engine::found_url::UrlSource;
use crate::engine::found_urls::FoundUrls;
use crate::engine::parsed_url::ParsedUrl;
use crate::types::ContentTypeId;
use crate::utils;

static RE_CSS_URL: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?im)url\s*\(\s*["']?([^"')]+)["']?\s*\)"#).unwrap());

static RE_IS_IMAGE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\.(jpg|jpeg|png|gif|webp|avif|svg|ico|tif|bmp)(\?.*|#.*)?$").unwrap());

static RE_IS_FONT: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.(eot|ttf|woff2|woff|otf)(\?.*|#.*)?$").unwrap());

static RE_IS_CSS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.css(\?.*|#.*)?$").unwrap());

static RE_CSS_URL_OFFLINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)url\((['"]?)((?:[^'")\s]|\([^)]*\))+)['"]?\)"#).unwrap());

pub struct CssProcessor {
    config: ProcessorConfig,
    debug_mode: bool,
    relevant_content_types: Vec<ContentTypeId>,
}

impl CssProcessor {
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            debug_mode: false,
            relevant_content_types: vec![ContentTypeId::Html, ContentTypeId::Stylesheet],
        }
    }

    /// Remove unwanted code from CSS based on disable options
    fn remove_unwanted_code_from_css(&self, css: &str) -> String {
        let mut result = css.to_string();

        if !self.config.fonts_enabled {
            result = utils::strip_fonts(&result);
        }
        if !self.config.images_enabled {
            result = utils::strip_images(&result, None);
        }

        result
    }

    /// The `url()` references of `content`. In a stylesheet (`every_url`) each one is a file the
    /// browser loads; one without an image, font or CSS extension counts as an image. In HTML,
    /// where the pattern also matches script and text, only those with such an extension are kept.
    fn find_css_urls(&self, content: &str, source_url: &ParsedUrl, every_url: bool) -> Option<FoundUrls> {
        let source_url_str = source_url.get_full_url(true, false);

        // Find all url() references in CSS
        let mut found_urls = FoundUrls::new();
        for caps in RE_CSS_URL.captures_iter(content) {
            if let Some(m) = caps.get(1) {
                // `url( x )` and `url(" x ")`: the whitespace around the URL is not part of it
                let url = m.as_str().trim_matches(|c: char| c.is_ascii_whitespace());
                if url.is_empty() {
                    continue;
                }
                let is_font = RE_IS_FONT.is_match(url);
                let is_css = RE_IS_CSS.is_match(url);
                if !is_font && !is_css && !RE_IS_IMAGE.is_match(url) {
                    // Not a file to load: `url(var(--x))` is an invalid token, which browsers
                    // ignore, and `@namespace url(…)` only names an XML namespace.
                    let statement = content[..m.start()].rsplit([';', '{', '}']).next().unwrap_or("");
                    let is_namespace = statement.trim_start().to_ascii_lowercase().starts_with("@namespace");
                    if !every_url || url.contains('(') || is_namespace {
                        continue;
                    }
                }
                let is_image = !is_font && !is_css;

                if (self.config.images_enabled && is_image)
                    || (self.config.fonts_enabled && is_font)
                    || (self.config.styles_enabled && is_css)
                {
                    found_urls.add_url_with_details(url, m.start(), None, &source_url_str, UrlSource::CssUrl);
                }
            }
        }

        if found_urls.get_count() > 0 {
            Some(found_urls)
        } else {
            None
        }
    }
}

impl ContentProcessor for CssProcessor {
    fn find_urls(&self, content: &str, source_url: &ParsedUrl) -> Option<FoundUrls> {
        self.find_css_urls(content, source_url, false)
    }

    fn find_urls_for_content_type(
        &self,
        content: &str,
        content_type: ContentTypeId,
        source_url: &ParsedUrl,
    ) -> Option<FoundUrls> {
        self.find_css_urls(content, source_url, content_type == ContentTypeId::Stylesheet)
    }

    fn apply_content_changes_before_url_parsing(
        &self,
        _content: &mut String,
        _content_type: ContentTypeId,
        _url: &ParsedUrl,
    ) {
        // No changes needed before URL parsing in CssProcessor
    }

    fn apply_content_changes_for_offline_version(
        &self,
        content: &mut String,
        _content_type: ContentTypeId,
        url: &ParsedUrl,
        _remove_unwanted_code: bool,
    ) {
        *content = RE_CSS_URL_OFFLINE
            .replace_all(content, |caps: &regex::Captures| {
                let quote = caps.get(1).map_or("", |m| m.as_str());
                let found_url = caps.get(2).map_or("", |m| m.as_str());

                // If data URI, anchor, or non-requestable resource, skip
                if !utils::is_href_for_requestable_resource(found_url) || found_url.starts_with('#') {
                    return caps.get(0).map_or("", |m| m.as_str()).to_string();
                }

                let relative_url = convert_url_to_relative(url, found_url, None, &self.config);
                format!("url({}{}{})", quote, relative_url, quote)
            })
            .to_string();

        *content = self.remove_unwanted_code_from_css(content);
    }

    fn set_stored_url(&mut self, stored_url: StoredUrlFn) {
        self.config.stored_url = Some(stored_url);
    }

    fn is_content_type_relevant(&self, content_type: ContentTypeId) -> bool {
        is_relevant(content_type, &self.relevant_content_types)
    }

    fn get_name(&self) -> &str {
        "CssProcessor"
    }

    fn set_debug_mode(&mut self, debug_mode: bool) {
        self.debug_mode = debug_mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> ProcessorConfig {
        ProcessorConfig::new(ParsedUrl::parse("https://example.com/", None))
    }

    #[test]
    fn test_find_css_urls() {
        let processor = CssProcessor::new(make_config());
        let css = r#"
            body { background: url('/img/bg.jpg'); }
            @font-face { src: url('/fonts/custom.woff2'); }
        "#;
        let source = ParsedUrl::parse("https://example.com/style.css", None);
        let result = processor.find_urls(css, &source);
        assert!(result.is_some());
        assert!(result.unwrap().get_count() >= 2);
    }

    #[test]
    fn css_urls_carry_their_offset() {
        let processor = CssProcessor::new(make_config());
        let css = "a{background:url( '/img/a.png' )}\n@import url(\"/b.css\");";
        let source = ParsedUrl::parse("https://example.com/style.css", None);
        let found = processor.find_urls(css, &source).unwrap();
        for url in ["/img/a.png", "/b.css"] {
            let found_url = found.get_urls().values().find(|u| u.url == url).unwrap();
            assert_eq!(found_url.offset, css.find(url), "{url}");
            assert_eq!(found_url.text, None);
        }
    }

    #[test]
    fn test_find_css_urls_disabled_images() {
        let mut config = make_config();
        config.images_enabled = false;
        let processor = CssProcessor::new(config);
        let css = r#"body { background: url('/img/bg.jpg'); }"#;
        let source = ParsedUrl::parse("https://example.com/style.css", None);
        let result = processor.find_urls(css, &source);
        // Should be None because images are disabled
        assert!(result.is_none());
    }

    fn sorted_urls(found: Option<FoundUrls>) -> Vec<String> {
        let mut urls: Vec<String> = found
            .map(|found| found.get_urls().values().map(|u| u.url.clone()).collect())
            .unwrap_or_default();
        urls.sort();
        urls
    }

    #[test]
    fn every_url_of_a_stylesheet_is_found_but_html_keeps_the_extension_filter() {
        let processor = CssProcessor::new(make_config());
        let css = "a{background:url(bg/pattern)} b{background:url('/img?id=3')} c{cursor:url(x.cur),auto} \
                   d{background:url(data:image/png;base64,AAAA)} e{fill:url(#frag)} f{background:url(about:blank)} \
                   g{background:url(/a.png)} h{background:url(var(--x))} \
                   @namespace svg url(http://www.w3.org/2000/svg); @NAMESPACE url(http://www.w3.org/1999/xhtml);";
        let source = ParsedUrl::parse("https://example.com/css/style.css", None);
        assert_eq!(
            sorted_urls(processor.find_urls_for_content_type(css, ContentTypeId::Stylesheet, &source)),
            ["/a.png", "/img?id=3", "bg/pattern", "x.cur"]
        );

        let html = format!(
            "<html><head><style>{css}</style><script>var u = new URL(location.href);</script></head>\
             <body><div style=\"background:url(bg/inline)\"></div><p>Use url(path-in-prose) in CSS.</p></body></html>"
        );
        let page = ParsedUrl::parse("https://example.com/", None);
        assert_eq!(
            sorted_urls(processor.find_urls_for_content_type(&html, ContentTypeId::Html, &page)),
            ["/a.png"]
        );
    }

    #[test]
    fn a_stylesheet_url_without_a_known_extension_counts_as_an_image() {
        let mut config = make_config();
        config.images_enabled = false;
        let processor = CssProcessor::new(config);
        let css = "a{background:url(bg/pattern)} @font-face{src:url(/f.woff2)}";
        let source = ParsedUrl::parse("https://example.com/style.css", None);
        assert_eq!(
            sorted_urls(processor.find_urls_for_content_type(css, ContentTypeId::Stylesheet, &source)),
            ["/f.woff2"]
        );
    }

    #[test]
    fn css_urls_surrounded_by_whitespace_are_found_without_it() {
        let processor = CssProcessor::new(make_config());
        let css = r#"a { background: url( /img/a.png ); } b { background: url(" /img/b.png "); }
            @font-face { src: url( '/fonts/c.woff2' ), url(	/fonts/d.woff
            ); }"#;
        let source = ParsedUrl::parse("https://example.com/style.css", None);
        let found = processor.find_urls(css, &source).expect("urls found");
        let mut urls: Vec<&str> = found.get_urls().values().map(|u| u.url.as_str()).collect();
        urls.sort();
        assert_eq!(urls, ["/fonts/c.woff2", "/fonts/d.woff", "/img/a.png", "/img/b.png"]);
    }
}
