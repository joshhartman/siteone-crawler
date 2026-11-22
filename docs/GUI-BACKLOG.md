# GUI backlog — crawler-side changes

Changes to the crawler (`src/`) that would improve the GUI. None of them is made: the GUI is
built on the crawler as it is. Each entry says what the GUI does instead today.

## Link data

1. **Referring page on every crawled URL.** Add `source` (URL of the page or stylesheet that
   referenced it), `sourceAttr` (`<a href>`, `<img src>`, `css url()`, …) and `external` to the
   `url` event (additive, protocol 1). The crawler already has them per URL
   (`VisitedUrl::source_uq_id`, `source_attr`, `is_external`).
   *Today:* "Found on" is joined from the result JSON's `404` table, so it exists for 404s only
   and only after the crawl. A 403, 5xx, timeout or connection failure on the site has none.

2. **All referring pages, link counts and depth per URL.** The crawler keeps only the first
   page a URL was found on. Reporting every referrer, the number of in-links and out-links, and
   the crawl depth would allow a "pages linking to this one" list and In links / Out links /
   Level columns (as in Xenu's Link Sleuth), and a site map view.
   *Today:* not available for internal URLs. External links carry the sources the
   `external-urls` table lists.

3. **Link text.** Capture the anchor text (or image alt) when a link is extracted.
   *Today:* the GUI requests the referring page again after the crawl and reads the link text
   from it — extra requests to the crawled site.

4. **A link-check mode for external links.** Request each external link once without parsing
   the page (optionally reading its title), instead of `--allowed-domain-for-crawling=*` +
   `--single-foreign-page`, which crawls the external page and its files. On a real site that
   turned 471 URLs into 2,877 with 966 unrelated failures and requested URLs found inside
   third-party scripts.
   *Today:* the GUI checks external links itself after the crawl; the CLI has no equivalent.

   A list of URLs to request (`--url-list` or several `--url`) would also serve the retry queue,
   which today starts one crawler process per retried URL.

5. **Reports that follow a retry.** The crawler can only report the crawl it has just made: there
   is no way to rebuild the HTML report, the JSON and the scores from stored results with a few
   answers replaced.
   *Today:* a retry updates the app's views and the app's own link report; the crawler's reports
   stay as the crawl wrote them until the site is crawled again (the HTTP cache makes that cheap;
   see 9 for the retried answers it does not carry over).

6. **Content type of a failed asset.** A missing image is reported with `contentType` "HTML"
   (the server's 404 page).
   *Today:* the GUI classifies pages and assets by the URL's file extension.

## Coverage

14. **Count what the crawl left out.** URLs that match `--ignore-regex` (or miss `--include-regex`),
    lie beyond `--max-depth`, or were found after `--max-visited-urls` / `--max-queue-length` were
    reached are dropped without a trace (`Crawler::is_url_suitable_for_queue_static`; the
    `SkippedReason::ExceedsMaxDepth` variant exists but is never recorded). Recording them in the
    skip list with their own reasons (`Ignore regex`, `Max depth`, `Max URLs`), and stating in the
    result JSON whether a limit cut the crawl short (the manager's `CrawlEnd`, today used only by
    `--ai-geo`), would let *Not checked* give numbers.
    *Today:* the link report's *Not checked* names these gaps without a count, and infers the URL
    limit from "URLs crawled ≥ `--max-visited-urls`".

16. **Schemes with a hyphen, a plus sign, a dot or a digit.** The filter for non-http links
    (`NON_HTTP_SCHEME_RE` in `src/engine/found_urls.rs`) accepts letters only in the scheme, so
    `android-app://…`, `ms-windows-store://…`, `fb-messenger://…` or `web+app:…` is taken for a
    relative path, requested as `<page>/android-app://…` and reported as a 404 that no visitor
    can meet. RFC 3986 allows `[a-z][a-z0-9+.-]*`.
    *Today:* such a link shows as a broken internal page; the user adds an `--ignore-regex`.

## Requests

7. **Default User-Agent.** The desktop default is `Chrome/<two-digit year>.0.0.0` (currently
   `Chrome/26`). Some hosts close the connection for such an old version: a default crawl of
   one WordPress site ended with "no pages crawled" (status -4), while `Chrome/142` worked.
   *Today:* the GUI's "No pages crawled" message suggests setting `--user-agent`.

8. **`Crawl-delay` in robots.txt** is not honoured.
   *Today:* the user lowers `--max-reqs-per-sec` by hand.

   **A host that does not answer during the crawl.** The crawler requests every URL of a host
   although the host has stopped answering, and waits for the timeout each time (with the app's
   30 s default, 40 assets on a dead allowed host cost minutes). Giving a host up after a few
   failures without any answer would end that.
   *Today:* the app does this only for its own requests (the external link check and retries).

9. **The HTTP cache key and the `Origin` header.** `Origin` is part of the cache key. The crawler
   sends it with a non-image URL found on a page only when that page is already recorded in
   `Status` at that moment (`get_origin_header_value_by_source_uq_id`), so the same URL of the same
   crawl is requested with or without it depending on timing, and a start URL never has it. One URL
   therefore has up to two cache entries, and which one a crawl reads or writes is not
   deterministic: a repeated crawl misses entries the earlier one wrote, or reuses a stored 404
   although a newer answer is stored under the other key. A key without `Origin` (or a header that
   does not depend on timing) would fix both.
   *Today:* a retry refreshes the entry without `Origin`: a later crawl always sees a retried image
   or start page; for a page found as a link it may see the retried answer, request the page again,
   or reuse its own stored 404 or 403 for the cache's lifetime (GUI-SPEC §4.9).

10. **A remote browser and a fetch service for sites that turn the crawler away.** The crawler
    fetches directly or through a Chromium it launches itself (`Browser::launch`); `--proxy` takes
    `host:port` only.
    - *Remote browser:* an option such as `--browser-ws-endpoint=<ws://…>` that connects to a
      running Chromium (chromiumoxide's `Browser::connect`) instead of launching one — for a hosted
      browser such as Browserless.io.
    - *Fetch service:* another `Fetcher` (`src/engine/fetcher.rs`) that asks a service for each
      page — FlareSolverr's JSON API (`POST /v1`, `request.get`) returns the status, headers, HTML
      and cookies. A scrape API that returns only page content (Firecrawl) would leave the header,
      caching and security analyzers without data.
    - *Proxy credentials:* `--proxy` with `user:password@host:port`.

    *Today:* the user sets a current `--user-agent`, turns on `--browser`, lowers the request
    rate, or allowlists the crawler in the site's firewall. A clearance cookie and User-Agent taken
    from FlareSolverr by hand can be passed with `--header` and `--user-agent` (same IP, until the
    cookie expires).

## Project

11. **Tests on Windows.** 98 crawler tests pass `--config-file=/dev/null`, which does not exist
   on Windows, so root `cargo test` fails there (it passes on Linux CI). An empty temporary
   file would make them portable.

12. **`wizard::presets` is private.** `pub mod presets;` would let the GUI import the presets.
   *Today:* the GUI compiles `src/wizard/presets.rs` by path.

13. **JSON output documentation.** `docs/JSON-OUTPUT.md` names the category code
    `bestPractices`; the output has `best-practices`. The summary status `NOTICE` is not listed.

## GUI features not built

15. **Broken page anchors.** Check that the `#fragment` of a link exists as an `id` (or `name`)
    on its target page, as the "broken page-local links" section of a Xenu report does. As an
    opt-in switch like *Check external links*: after the crawl the app would read each crawled
    HTML page once, collect its links with a fragment and its ids, and list links whose target
    has no such id in the Link checker and the link report. Cost: one extra request per crawled
    HTML page, unless the crawler reports fragments and ids itself (which would also remove the
    false alarms for ids that a script adds). Fragments of script routers (`#/path`, `#!`) and a
    bare `#` would be left out.
