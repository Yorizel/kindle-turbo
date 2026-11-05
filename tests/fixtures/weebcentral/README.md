# WeebCentral fixture provenance

The HTML shapes were checked with direct HTTP requests on 2026-11-05 against:

- `https://weebcentral.com/search/data?text=return%20of%20the%20disaster&limit=32&offset=0&display_mode=Full+Display`
- `https://weebcentral.com/search/data?text=&limit=32&offset=0&display_mode=Full+Display&sort=Popularity`
- `https://weebcentral.com/series/01J76XYF85TPSFGEH5GCC1VWAW`
- `https://weebcentral.com/series/01J76XYF85TPSFGEH5GCC1VWAW/full-chapter-list`
- `https://weebcentral.com/chapters/01J76XZ5Q84XEMX0NDR6SKRCSM/images?is_prev=False&reading_style=long_strip`

These are reduced synthetic fixtures using the observed HTML structure, not complete captured pages. The first series ID/title is real. Other series labels, chapter IDs, decimal/Volume/Special cases and image paths are synthetic tests for identity preservation, entity decoding and selection; they do not assert live availability of those records. No chapter images, descriptions, scripts, user data or credentials are included.

The frontend needs a browser-shaped Accept header. Image requests use the source Referer and image Accept header, without Origin. Series and chapter identity comes from the opaque URL ID, not from a numeric title. Pagination uses the offset in the HTMX button. The adapter spaces requests by two seconds.
