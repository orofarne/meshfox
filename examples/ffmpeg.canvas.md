<!-- meshfox:canvas -->
# Video toolbox (ffmpeg)
<!-- meshfox:node id="root" -->

A small `ffmpeg` how-to that doubles as a tour of meshfox's variable types
and forms (see SPEC.md's "Variables", "File variables" and "Form fences").
Everything you would otherwise retype on a command line is a variable, and
one form sets them all:

| Variable | Type | What it shows |
| --- | --- | --- |
| `INPUT` | `file` (`mode="open"`, `ext=` hint) | pick an existing video — **Browse…** (web) / `Ctrl-O` (terminal) |
| `OUTPUT` | `file` (`mode="save"`) | name a file that does not exist yet |
| `OUTPUT_DIR` | `dir` | a folder; `@tmp` is the canvas's own scratch directory |
| `PRESET` | `select` | x264 speed/size trade-off |
| `CRF` | `int` | quality, validated as a whole number |
| `KEEP_AUDIO` | `bool` | a checkbox |
| `START`, `DURATION` | `string` | free text |

**Needs `ffmpeg` and `ffprobe` on `PATH`.** Out of the box everything works
on a generated test clip (block `sample`); pick your own video with
**Browse…** and the same blocks run on it.

<!-- meshfox:var name="INPUT" type="file" ext="mp4,mkv,mov,webm,avi" prompt="Input video" default="@tmp/out/sample.mp4" -->
<!-- meshfox:var name="OUTPUT" type="file" mode="save" ext="mp4,mkv" prompt="Converted video" default="@tmp/out/converted.mp4" -->
<!-- meshfox:var name="OUTPUT_DIR" type="dir" prompt="Folder for the probe report, audio and GIF" default="@tmp/out" -->
<!-- meshfox:var name="PRESET" type="select" choices="ultrafast,fast,medium,slow" default="fast" -->
<!-- meshfox:var name="CRF" type="int" prompt="Quality (CRF — lower is better, 18–28 is sensible)" default="23" -->
<!-- meshfox:var name="KEEP_AUDIO" type="bool" prompt="Keep the audio track" default="true" -->
<!-- meshfox:var name="START" prompt="Start (seconds or hh:mm:ss)" default="0" -->
<!-- meshfox:var name="DURATION" prompt="Duration in seconds (empty: to the end; GIF: 5 when empty)" default="" -->

## Test clip
<!-- meshfox:node id="sample" -->

Ten seconds of a generated test pattern with a tone, written to
`OUTPUT_DIR` (once — it is left alone if it is already there). It is what
`INPUT` points at by default; `convert` below runs it first, so you never
have to. Pick a video of your own with **Browse…** and it is simply unused.

```bash name="sample" env="$OUTPUT_DIR"
set -euo pipefail
mkdir -p "$OUTPUT_DIR"
if [ -f "$OUTPUT_DIR/sample.mp4" ]; then
  echo "$OUTPUT_DIR/sample.mp4 is already there"
  exit 0
fi
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=640x360:rate=25" \
  -f lavfi -i "sine=frequency=440:sample_rate=44100" \
  -t 10 -c:v libx264 -pix_fmt yuv420p -c:a aac "$OUTPUT_DIR/sample.mp4"
echo "wrote $OUTPUT_DIR/sample.mp4"
```

## Inspect
<!-- meshfox:node id="inspect" -->

`ffprobe` prints the container and streams as JSON into `OUTPUT_DIR`. The
file node below it reads that report back through `$OUTPUT_DIR` — a `dir`
variable's value is a trusted root, so a path outside the canvas directory
is fine once the folder is confirmed.

```bash name="probe" env="$INPUT,$OUTPUT_DIR"
set -euo pipefail
[ -f "$INPUT" ] || { echo "input not found: $INPUT (run 'sample' or pick a video)" >&2; exit 1; }
mkdir -p "$OUTPUT_DIR"
ffprobe -v error -show_entries format=format_name,duration,size,bit_rate:stream=index,codec_type,codec_name,width,height \
  -of json "$INPUT" > "$OUTPUT_DIR/probe.json"
echo "wrote $OUTPUT_DIR/probe.json"
```

### Report
<!-- meshfox:node id="probe-report" type="file" display="code" -->

[probe.json]($OUTPUT_DIR/probe.json)

## Convert
<!-- meshfox:node id="convert" -->

The form sets the variables; **Apply** commits them for the session and —
because `convert` is marked `autorun` — re-encodes right away, no run
button. Nothing else reacts: `probe`, `extract-audio` and `gif` read the
very same values (`INPUT`, `OUTPUT_DIR`, `START`, `DURATION`) when you run
them. The default paths live under `@tmp` — the canvas's scratch
directory, which `session reset` clears — so they need no confirmation; a
file or folder you choose elsewhere becomes a trusted location for this
canvas once chosen.

```form name="settings" send="Apply"
field var="INPUT" label="Input video"
field var="OUTPUT" label="Converted video"
field var="OUTPUT_DIR" label="Output folder"
field var="PRESET" label="x264 preset"
field var="CRF" label="Quality (CRF)"
field var="KEEP_AUDIO" label="Keep audio"
field var="START" label="Start"
field var="DURATION" label="Duration (s)"
```

Re-encodes to H.264 with the chosen preset and quality, optionally cutting
`START`/`DURATION` out first. `OUTPUT` is a `mode="save"` file: its folder
is created, the file itself is overwritten without asking.

```bash name="convert" deps="sample/sample" autorun env="$INPUT,$OUTPUT,$PRESET,$CRF,$KEEP_AUDIO,$START,$DURATION"
set -euo pipefail
[ -f "$INPUT" ] || { echo "input not found: $INPUT (run 'sample' or pick a video)" >&2; exit 1; }
mkdir -p "$(dirname "$OUTPUT")"
args=(-y -hide_banner -loglevel error -ss "$START")
[ -n "$DURATION" ] && args+=(-t "$DURATION")
args+=(-i "$INPUT" -c:v libx264 -preset "$PRESET" -crf "$CRF" -pix_fmt yuv420p)
if [ "$KEEP_AUDIO" = "true" ]; then args+=(-c:a aac); else args+=(-an); fi
ffmpeg "${args[@]}" "$OUTPUT"
echo "wrote $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
```

## Audio
<!-- meshfox:node id="audio" -->

The audio track alone, as MP3, named after the input.

```bash name="extract-audio" env="$INPUT,$OUTPUT_DIR"
set -euo pipefail
[ -f "$INPUT" ] || { echo "input not found: $INPUT (run 'sample' or pick a video)" >&2; exit 1; }
mkdir -p "$OUTPUT_DIR"
name="$(basename "${INPUT%.*}")"
ffmpeg -y -hide_banner -loglevel error -i "$INPUT" -vn -c:a libmp3lame -q:a 2 "$OUTPUT_DIR/$name.mp3"
echo "wrote $OUTPUT_DIR/$name.mp3"
```

## GIF
<!-- meshfox:node id="gif" -->

A looping GIF of the same `START`/`DURATION` window (five seconds when
`DURATION` is empty), with a generated palette so it does not look like
mud.

```bash name="gif" env="$INPUT,$OUTPUT_DIR,$START,$DURATION"
set -euo pipefail
[ -f "$INPUT" ] || { echo "input not found: $INPUT (run 'sample' or pick a video)" >&2; exit 1; }
mkdir -p "$OUTPUT_DIR"
name="$(basename "${INPUT%.*}")"
ffmpeg -y -hide_banner -loglevel error -ss "$START" -t "${DURATION:-5}" -i "$INPUT" \
  -vf "fps=12,scale=480:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" \
  "$OUTPUT_DIR/$name.gif"
echo "wrote $OUTPUT_DIR/$name.gif"
```
