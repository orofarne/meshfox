<!-- meshfox:canvas -->
# Browsing tables

A `file` node with `display="table"` shows its target as an interactive,
read-only table instead of a link or a block of text — a CSV, a TSV, a Parquet
file, JSON lines, anything the [`duckdb`](https://duckdb.org) command-line tool
can read (also compressed: `.csv.gz`). The file is never loaded into the page:
meshfox asks `duckdb` for the rows on screen, so a file with millions of rows
opens as quickly as one with twenty.

**Needs the `duckdb` executable.** meshfox never installs it for you: put it
on `PATH`, or set `duckdb_path` under `[tables]` in `.meshfox/config.toml` (or
the `MESHFOX_DUCKDB` environment variable). Without it a table node says so
instead of showing a table.

What you can do with a table, in the browser and — full screen, with `Enter` —
in the terminal viewer (`meshfox tui`):

- **Sort** — click a column header (none → ascending → descending), Shift-click
  to add a second key; `s`/`S` in the terminal.
- **Filter a column** — type into the box under its header (`f` in the
  terminal): plain text *contains* it (numbers: *equals*); `>10`, `>=10`,
  `<10`, `=x`, `!=x`; `^start`, `$end`, `~anywhere`; `null`, `!null`.
- **Search every column** at once.
- **Look at one cell** — click it to read the whole value (`Enter` in the
  terminal), `y`/Cmd-C copies it.

The table is read-only: edit the file with any editor and the node notices the
change and reloads. `meshfox static` and `meshfox pdf` print the first 100 rows
as a plain grid — and **fail** if a table can't be read (no `duckdb`, no file),
rather than quietly leaving it out.

## A small CSV
<!-- meshfox:node id="products" type="file" display="table" w=860 h=420 -->

[products.csv](tables-data/products.csv)

Twenty-four products, committed next to this canvas. Try filtering `price`
with `>=50`, `category` with `kitchen`, or `note` with `!null`. Empty cells are
NULL.

## A million rows, generated
<!-- meshfox:node id="events-source" -->

The big files below don't live in the repository — this block makes one.
Run it (`r` in the terminal, the run button in the browser), then watch
`events.csv` in the next node fill in: a million rows, sorted, filtered and
searched in a fraction of a second after the first, one-time import
(which meshfox keeps in `.meshfox/tables/`, capped by `[tables]
cache_max_bytes`, 4 GiB by default).

```bash name="generate" outputs="tables-data/events.csv"
mkdir -p tables-data
awk 'BEGIN {
  srand(42)
  n = split("EU US APAC LATAM MEA", regions, " ")
  print "id,ts,region,amount,note"
  for (i = 1; i <= 1000000; i++) {
    printf "%d,2026-%02d-%02d %02d:%02d,%s,%.2f,%s\n", i, int(rand()*12)+1, \
      int(rand()*28)+1, int(rand()*24), int(rand()*60), \
      regions[int(rand()*n)+1], rand()*1000, (i % 7 == 0 ? "" : "order " i)
  }
}' > tables-data/events.csv
echo "wrote $(wc -l < tables-data/events.csv) lines"
```

## events.csv
<!-- meshfox:node id="events" type="file" display="table" w=860 h=480 -->

[events.csv](tables-data/events.csv)

Until `generate` has run this says the file is missing; afterwards, sort
`amount` descending, filter `region` with `APAC` and `ts` with `2026-06`, or
scroll to the very last row with `G` in the terminal. A column's type
(`bigint`, `varchar`, `double`, ...) is detected from the data and shown under
its name; numbers are right-aligned.

## The same data as Parquet
<!-- meshfox:node id="events-parquet-source" -->

Parquet needs no import at all — meshfox reads it in place — so it's the
cheapest format for really big data. This block turns the CSV into one with
`duckdb` itself (the same tool the table view uses):

```bash name="to-parquet" deps="events-source/generate" inputs="tables-data/events.csv" outputs="tables-data/events.parquet"
duckdb -c "COPY (SELECT * FROM 'tables-data/events.csv') TO 'tables-data/events.parquet' (FORMAT parquet)"
```

## events.parquet
<!-- meshfox:node id="events-parquet" type="file" display="table" w=860 h=420 -->

[events.parquet](tables-data/events.parquet)

Same rows, a fraction of the disk space — compare the file size in the status
bar tooltip with the CSV's.
