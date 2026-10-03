<!-- meshfox:canvas -->
# artifacts

File inputs and outputs form an executable pipeline. Run the `pipeline` button twice: the first run creates a source CSV, transforms it and copies the result; the second skips unchanged dependencies. Edit a row in `artifact-demo/source.csv` and run again to invalidate the transform. Delete either output to restore it.

`deps=` expresses execution cascade. `inputs=` instead discovers the producer through its `outputs=` and observes the file content. No explicit dependency is needed between transform and copy. A directly requested executable always runs.

The fixture is intentionally small; the same declaration can include parser code, patches and downloaded CSV/JSON/ZIP files in a data-processing project. External database revisions remain ordinary variables produced by an `always` observation block.

## Seed
<!-- meshfox:node id="seed" -->

```sh name="seed" outputs="artifact-demo/source.csv"
mkdir -p artifact-demo
if [ ! -f artifact-demo/source.csv ]; then
  printf "name,value\nalpha,10\nbeta,20\n" > artifact-demo/source.csv
fi
```

## Transform
<!-- meshfox:node id="transform" -->

```sh name="transform" inputs="artifact-demo/source.csv" outputs="artifact-demo/result.csv"
awk -F, 'BEGIN { OFS="," } NR == 1 { print; next } { $2 = $2 * 2; print }' artifact-demo/source.csv > artifact-demo/result.csv
```

## Copy
<!-- meshfox:node id="copy" -->

```sh name="copy" inputs="artifact-demo/result.csv" outputs="artifact-demo/copied.csv"
cp artifact-demo/result.csv artifact-demo/copied.csv
```

## Pipeline
<!-- meshfox:node id="pipeline" -->

```button name="pipeline" deps="copy/copy"
Build the CSV pipeline
```

