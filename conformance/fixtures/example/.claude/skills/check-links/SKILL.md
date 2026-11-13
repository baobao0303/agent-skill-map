---
name: check-links
description: |
  Validates the portfolio's internal links before publishing. Walks
  every generated page and reports any link whose target is missing.
---

# check-links

The last gate before the site goes out. Link targets resolve per the [WHATWG URL standard](https://url.spec.whatwg.org/).

## Steps
1. List every HTML file under `public/`.
2. For each page, collect its internal links (every `href` to `/` or to a `.html` file).
3. Check the target exists under `public/` (treat `/` as `public/index.html`).
4. Report any link whose target is missing; if none, report "0 broken links".
