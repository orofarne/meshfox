<!-- meshfox:canvas -->
# Images with transparency
<!-- meshfox:node id="root" -->

PNG and WebP images can carry an alpha channel. meshfox keeps it: the image is
drawn over whatever is behind it, so one file works on a light page, a dark
page and in a terminal. The two files below are the same picture (a capsule
fading to the right, an opaque ring and a half-transparent disc), made once
for this example (they fall under this repository's own license):

- [images-data/alpha.png](./images-data/alpha.png) — PNG, RGBA
- [images-data/alpha.webp](./images-data/alpha.webp) — WebP, lossless, RGBA

## PNG
<!-- meshfox:node id="png" -->

![A fading capsule, a ring and a translucent disc](./images-data/alpha.png)

## WebP
<!-- meshfox:node id="webp" -->

![The same picture as WebP](./images-data/alpha.webp)

## Over a background of its own
<!-- meshfox:node id="backed" -->

`{bg=…}` right after an image paints a color behind it where it is
transparent (see MARKDOWN.md, "Image size and background"); it is for SVG
diagrams drawn for a white page, but it is also the way to pin a raster to one
backdrop whatever the theme is:

![On white](./images-data/alpha.png){width=50% bg=#fff}

![On a dark slate](./images-data/alpha.webp){width=50% bg=#2b303b}

## How each viewer shows it
<!-- meshfox:node id="viewers" -->

| Viewer | Transparency |
|---|---|
| web UI | the browser composites it over the page |
| TUI | the terminal protocols (iTerm2, Sixel) paint transparent pixels black, so the TUI flattens the image onto the terminal's own background color (asked once with OSC 11) before sending it |
