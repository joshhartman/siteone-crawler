# GUI backlog — crawler-side changes

Changes to the crawler (`src/`) that would improve the GUI. Items 1–3 are done on both sides
(additive in the crawler: new event fields, new JSON keys and table, the `--link-details` option;
the GUI uses them and passes `--link-details` by default). For the others the GUI is built on the
crawler as it is. Each entry says what the GUI does instead today.

## Link data

1. **Referring page on every crawled URL.** *Done in the crawler.* Every `url` event has
   `source` (the page, stylesheet, script, sitemap or redirecting URL whose reference queued the
   URL — the page itself, not its `<base href>`; absent for a start URL), `sourceAttr`
   (`<a href>`, `<img src>`, `css url()`, `redirect`, …), `external` and `depth`, also without
   `--link-details` ([EVENTS.md](EVENTS.md#url)). With `--link-details` the result JSON's
   `results` rows carry the same facts.
   *Done in the GUI:* "Found on" (and *By page*) comes from the event as each URL arrives, for
   every status (timeouts, 403, 5xx, connection failures); the URLs tab has a *Depth* column from
   it. The `404` table join stays as the fallback for runs of a crawler without the field, so "Page
   not reported by the crawler" remains only for start URLs and those runs.

2. **All referring pages, link counts and depth per URL.** *Done in the crawler, with
   `--link-details`.* The result JSON's `links` table has one row per (target, referring page),
   up to 50 per target in discovery order (the page that queued it first) with `inLinks`, the
   total, on every row — for crawled URLs and for those that were not crawled (external links,
   skipped files, robots.txt, ignore regex, limits). `results` rows get `inLinks`, `outLinks` and
   `depth` (shortest link distance from the start URL, exact after the crawl; the event's `depth`
   is the value known when the URL was crawled). See [JSON-OUTPUT.md](JSON-OUTPUT.md#529-links-links).
   *Done in the GUI:* `--link-details` is an option with an app default of on (Setup → Scope,
   "Record every referring page"; `--link-details=true` in every command, like `--timeout=30`).
   Every referring page of a broken link in the Link checker ("+N more", the kept pages in the
   tooltip, "and N more pages" = `inLinks` − rows), under each of its pages *by page*, in Copy list
   and the link report; *Pages linking here…* in the URLs tab's row menu; *Depth* (exact) and *In
   links* columns. The app's external link check takes its links and all their referrers from
   `links` — `external-urls` lists at most 5 pages per URL and stays the source for runs without
   the table. Out links is not shown.

3. **Link text.** *Done in the crawler, with `--link-details`.* Each `links` row has `linkText`
   (visible text of an `<a>`, or its image's `alt`; an `<img>`'s `alt`; ≤ 120 characters) and the
   position of the reference in the source as received (`sourceLine`, `sourceColumn`; none for a
   `--browser`-rendered page, sitemap or JavaScript references). The first reference is on the
   `url` event too (`linkText`, `sourceLine`, `sourceColumn`).
   *Done in the GUI:* the Link text column and the `:line` suffix of Found on come from the crawler
   (the event live, `links` for every referrer); a column beyond 500 is shown as well
   (`:1:20345`), since the crawler reports no line length. The app reads a referring page itself
   only for a reference without a crawler position that it can locate (an HTML attribute or a
   stylesheet `url()`: runs without link details, `--browser`-rendered pages) and for runs of an
   older crawler (their `404` table); a run with complete crawler data adds no request.

4. **A link-check mode for external links.** Request each external link once without parsing
   the page (optionally reading its title), instead of `--allowed-domain-for-crawling=*` +
   `--single-foreign-page`, which crawls the external page and its files. On a real site that
   turned 471 URLs into 2,877 with 966 unrelated failures and requested URLs found inside
   third-party scripts.
   *Today:* the GUI checks external links itself after the crawl; the CLI has no equivalent.

   The retry queue still starts one crawler process per retried URL. `--url-list` exists and was
   tested for it (`gui/tests/core_retry.rs`): one process with `--url-list`,
   `--max-visited-urls=N`, `--single-page`, `--workers=1` does send exactly one request per URL (no
   robots.txt, asset, link or redirect target) with a `url` event each, but it keeps neither rule of
   a pass: `--max-reqs-per-sec` spaces the *starts* of requests, so the request after a slow answer
   follows it at once (a pass waits 500 ms after the end of a request to a host), and it requests
   every URL of a host that does not answer (a pass gives it up after two failures). A link-check
   mode, or a per-host delay measured from the end of a request plus giving up a dead host, would
   make one process per pass possible.

5. **Reports that follow a retry.** The crawler can only report the crawl it has just made: there
   is no way to rebuild the HTML report, the JSON and the scores from stored results with a few
   answers replaced.
   *Today:* a retry updates the app's views and the app's own link report; the crawler's reports
   stay as the crawl wrote them until the site is crawled again (the HTTP cache makes that cheap;
   see 9 for the retried answers it does not carry over).

6. **Content type of a failed asset.** A missing image is reported with `contentType` "HTML"
   (the server's 404 page).
   *Today:* the GUI classifies pages and assets by the URL's file extension. With link data the
   `sourceAttr` of the referrers could decide instead (`<img src>` → image, `<script src>` →
   script, `<link href>` → stylesheet or other, `css url()` → image or font, `<a href>` → page or
   download); the extension would still be needed for `<a href>` to a file and for `<link href>`.

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

16. **Schemes with a hyphen, a plus sign, a dot or a digit.** *Done in the crawler.* The filter
    for non-http links follows RFC 3986 (`[a-z][a-z0-9+.-]*:`), so `android-app://…`,
    `ms-windows-store://…` or `web+app:…` is no longer taken for a relative path and requested.
    Fixed together with four other link defects: whitespace around a URL attribute
    (`href=" /x"` was requested as `%20/x`), `<base href>` recorded as the source page of a link,
    a race that could queue a URL twice, and the `<a href>` pattern matching other tags whose
    name starts with "a".

17. **Link defects left in the crawler** (seen while fixing 16, not fixed):
    - `utils::is_href_for_requestable_resource` and `RE_NON_HTTP_SCHEME` in the HTML processor
      still accept letters and digits only in a scheme; they are used by the offline export,
      which may rewrite an `android-app://` link as a file.
    - `base_processor::convert_url_to_relative` does not trim whitespace, so the offline and
      Markdown exports may not match `href=" /x"` with the crawled `/x` (not verified).
    - `url(" /x.png")` with whitespace inside the quotes in an inline `<style>` is not found.
    - `href = "…"` with spaces around `=` is not found.
    - The `queue + visited` limit checks are not atomic, so the URL limits are approximate.

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
