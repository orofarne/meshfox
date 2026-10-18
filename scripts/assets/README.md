# Social preview

`social-preview.svg` is the editable source for the archive site's shared
1200×630 social card. Regenerate the committed PNG with librsvg:

```sh
rsvg-convert scripts/assets/social-preview.svg -o site-template-archive/social-preview-v1.png
```

Inspect the PNG after changing text or fonts. The rasterizer uses locally
installed Courier New, Georgia and monospace fallback fonts.

The archive template supplies Open Graph and Twitter metadata, including canonical
and Open Graph URLs from the exporter's `page_url` context variable. Set
`base_url` in `template.toml` to publish absolute page URLs. The Cloudflare build
uses the latest released CLI, so these template changes require a CLI release
with `page_url` support before deployment.

For a new image revision, change the filename in the template as well, so social
platforms can fetch a new image URL. Existing shared messages may retain cached
previews.
