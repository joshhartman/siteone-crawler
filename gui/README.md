# SiteOne Crawler GUI

A native Windows desktop front end (Rust, egui/eframe) for the `siteone-crawler` CLI. It configures
a crawl, runs the CLI as a child process, shows progress, URLs, broken links, AI activity and
console output live, presents scores, findings and analysis tables when the crawl ends, and keeps
a history of runs. Every setting maps to the CLI and the equivalent command line is always shown
(the one exception, *Check external links*, is a check the app makes itself after the crawl). The design and its rules are in [`docs/GUI-SPEC.md`](../docs/GUI-SPEC.md).

The GUI is a member of the repository's Cargo workspace (`default-members = ["."]`): plain
`cargo build/test/clippy` at the root still build only the crawler. Run the commands below from the
repository root.

## Build and run

Toolchain 1.96.0 (pinned by `rust-toolchain.toml`), MSVC.

```
cargo build -p siteone-crawler-gui                  # debug  → target\debug\siteone-crawler-gui.exe
cargo build --release -p siteone-crawler-gui        # release → target\release\siteone-crawler-gui.exe
cargo build --release                               # the crawler → target\release\siteone-crawler.exe
```

The GUI needs the crawler binary. It looks, in this order, at the path set in Settings, at
`siteone-crawler.exe` **next to the GUI's exe**, and on `PATH`. Building both in the same profile
puts them side by side in `target\<profile>\`, so `target\release\siteone-crawler-gui.exe` finds
`target\release\siteone-crawler.exe`. To distribute, copy both exes into one folder. Without a
crawler, Setup shows a banner with **Locate…** and Start stays disabled. Settings warns when the
crawler's version differs from the one the GUI was built with (its option list may differ).

A debug build opens a console window next to the GUI; a release build does not.

## Where data lives

| What | Where |
|---|---|
| Settings, working configuration, profiles | `%APPDATA%\siteone-crawler-gui\` |
| Runs (one folder each), HTTP and AI caches, `crash.log` | `%LOCALAPPDATA%\siteone-crawler-gui\` (the runs folder can be moved in Settings) |
| Passwords and API keys | Windows Credential Manager, service `siteone-crawler-gui` |

`SITEONE_GUI_DATA_DIR` replaces both folders with one (portable use, tests); a relative path is
read against the folder the GUI is started from. The GUI makes its data folder its working
directory. A run folder holds `run.json` (outcome, redacted command line, configuration without
secrets or `--header` values), `events.ndjson`, `console.log`, the result JSON, the HTML report
and any exports — and, once the app has checked them, `external-links.json` (the external link
check) and `link-texts.json` (link texts read from the source pages), `retries.json` (the retry
queue and the newest answers) and `link-report.html` (the app's report of the run's links).

## Tests

```
cargo test -p siteone-crawler-gui
```

runs everything:

- **Unit tests** in the source files (option schema vs. the CLI's options, argv round trip through
  the CLI's parser, events, run state, ANSI parsing, result JSON, storage, history, redaction).
- **Integration tests** `tests/core_*.rs`, `tests/ui_start.rs`, `tests/ui_run_e2e.rs`,
  `tests/ui_link_check_e2e.rs`, `tests/ui_retry_e2e.rs` with the real crawler: they build it for the same profile
  (`cargo build -p siteone-crawler`, a no-op when up to date) and crawl a small generated site served
  by the crawler's own `--serve-offline` on 127.0.0.1 — no network. The external link check and the
  link texts — and the retries of `tests/core_retry.rs` — run against a small HTTP server written in
  the tests (`tests/link_server`, on 127.0.0.1 and ::1).
- **UI tests** `tests/ui_*.rs` with `egui_kittest`: interactions on every page, and PNG snapshots
  of every page and tab (two sizes, light and dark) written to `gui/tests/snapshots/`. A snapshot
  is checked for size and for not being blank, not compared with a reference image; look at the
  PNGs to review the UI. The folder is git-ignored. Without a GPU adapter the rendering is skipped
  with a message.

Gates before a change is done:

```
cargo fmt -p siteone-crawler-gui -- --check
cargo clippy -p siteone-crawler-gui --all-targets -- -D warnings
cargo test -p siteone-crawler-gui
```

## Known limits

- **Request timeout:** the app's default is 30 seconds, the CLI's is 5; the app therefore passes
  `--timeout` on every run, and a pasted command without `--timeout` gets 30.
- **One crawl at a time** per window, and one GUI instance per data folder is assumed (two would
  share profiles, settings and the session file).
- **Check external links** (Setup → Scope) is a setting of the app, not of the crawler (the CLI
  has no equivalent, and the command does not change). The crawler never requests other sites.
  After the crawl the app requests each **link** to another site (`<a href>`) **once** — one `GET`,
  redirects followed (at most 10) — and reads only the **page title** of an HTML answer (at most
  256 KB); it never loads the page's images, scripts, styles or links. Images, scripts and styles
  that the site loads from other sites are counted but not checked. At most 2,000 links per run;
  4 at a time, one at a time per host, 500 ms apart. A server that never answers is asked twice
  (once when its DNS lookup fails); its other links are marked failed without a request. *Refused* (401, 403, 429) means the site turned
  an automated request away — the link may well work in a browser; *Broken* is any other 4xx/5xx or
  no answer (DNS, connection, TLS, timeout, too many redirects). The site's `--http-auth`,
  `--header` values and cookies are never sent to other sites. *Check external links now* in the
  Link checker tab checks any finished run, also one crawled with the switch off.
- **Found on** for crawled URLs comes from the result JSON's `404` table, so it is known only for
  404 responses and only once the run has finished; other 4xx, 5xx and connection failures are
  listed without it. The crawler keeps only the **first** page that referenced a URL, so a full
  "pages linking to this one" list (as in Xenu) is not available without a change to the crawler.
- **By page** (Link checker tab) groups the broken links under the pages they were found on, and
  the link report adds *Broken links by page*. Because of the limit above, the internal links other
  than 404s land in the last group, "Page not reported by the crawler".
- **What was checked** (link report; one line *Not checked* in the Link checker tab) lists what the
  run left out: robots.txt and other skip reasons with counts, files on other sites, links over the
  cap or not checked, URL limits, depth, single page, turned-off content types and skip patterns.
  The crawler does not count what `--ignore-regex`, `--max-depth` or `--max-visited-urls` kept out,
  so those lines name the gap without a number.
- **Skip cart and action links** (Setup → Scope, on by default) adds four patterns to the run's
  `--ignore-regex` (shown in the command): WooCommerce `add-to-cart`, `remove_item`, `undo_item` and
  the WordPress log-out link. Requesting them changes the site's state and checks nothing (on one
  shop they were 154 of 172 crawled pages, ~4 s each). Turn it off to crawl them.
- **Link text** is read by fetching the "found on" pages again after the crawl — only pages of the
  crawled site, each once, at most 200 per run. The run's own `--http-auth` and `--header` go only
  where the crawler sends them (not from https to http; for an IP address or `localhost` only to the
  start port); other pages of the site are fetched without them. A run from History that used them
  does not fetch its pages (History keeps no password; Setup's is never borrowed). A page that
  changed since the crawl, or a link built by a script, shows "not found on the page".
- **Retry** (Link checker tab, after the crawl): selected rows, or the retry queue, are requested
  again one by one — a crawled URL by the crawler (one process per URL, exactly one request: no
  robots.txt, no assets, no links, a redirect not followed), an external link by the app's check.
  The app's views, counts and link report take the new answers, so they show what is still broken (a
  URL that answers OK now leaves them; no list of retried URLs is kept); the crawler's HTML report, result
  JSON and scores stay as the crawl wrote them (the app says so and offers *Run again*). The retry
  stores its answer in the HTTP cache, but a later crawl is sure to see it only for images and the
  start page: depending on timing the crawler requests a page found as a link with an `Origin`
  header, which is part of the cache key, so it may reuse its own stored 404 of that page for the
  cache's lifetime (24 h by default). A run from History that used `--http-auth` or `--header`
  (History keeps neither) retries external links only; Setup's password is never used in its place.
- *Run again* from History cannot bring back `--header` values or passwords; it names them and
  clears Setup's passwords (the AI API key stays). *Run again* of the run just made restores its
  passwords from memory.
- **Importing** a config file or a command replaces the configuration including its passwords:
  `--http-auth`, `--mail-smtp-pass` and `--upload-password` not in the import are cleared; the AI
  API key stays unless the import has one. Saved profiles keep their passwords. A pasted command is
  read as PowerShell or as Bash, as chosen in the dialog (the two shells *Copy* writes for); cmd
  syntax is not read, and a PowerShell variable (`$name`, `$(…)`) is refused rather than guessed.
- **Relative input paths** (`--url-list`, `--ai-prompt-file`, `--ai-api-key-file`,
  `--ai-schema-file`, `--ci-baseline`, `--browser-path`, `--ffmpeg-path`) name files in
  `%LOCALAPPDATA%\siteone-crawler-gui\`; the command preview shows the absolute path the crawler
  gets. In an imported config file they name files next to that file. Relative output paths land in
  the run folder.
- **A CLI config file** (`~/.siteone-crawler.conf`, or the CLI's system file) applies to the CLI,
  not to the app: every crawler the app starts gets `--config-file=<data folder>\empty-config.conf`,
  so Setup's defaults are what runs. Import the file (*Import config file…*) to use it.
- **Autosave** writes the working configuration, its passwords and the settings in order; a failed
  write shows an error, stays unsaved and is tried again (after the next change, after a back-off,
  and on exit).
- Windows only is built and tested; there is no installer, auto-update or code signing.
