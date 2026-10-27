# Homebrew publication

The Release workflow calls `.github/workflows/homebrew.yml` after GitHub Release
publication succeeds. Only stable `vX.Y.Z` tags update `orofarne/homebrew-tap`.
The job downloads the four published CLI archives, computes their SHA-256 values,
generates `Formula/meshfox.rb`, tests installation on macOS, and pushes a commit
to the tap's `main` branch. It uses the `HOMEBREW_TAP_TOKEN` repository secret,
which needs Contents read/write access to `orofarne/homebrew-tap`.

The formula template is `meshfox.rb.in`. It installs the `homebrew` marker that
disables self-updating; standalone archives stay unchanged. Linux URLs and
checksums are generated too, but the publication job tests installation on macOS.

Publication jobs are serialized. Repeating an unchanged release creates no
commit, and an older release never replaces a newer version in the tap. Pushes
are ordinary fast-forward pushes; a conflicting external change fails the job
rather than overwriting the tap. A tap failure leaves the GitHub Release intact.

To retry publication without rebuilding binaries, open **Actions → Update
Homebrew tap → Run workflow**, choose the branch containing this workflow, and
enter an existing release tag such as `v0.26.1`. The tag's assets must already
be published. The token must be valid and the tap's `main` branch must allow it
to push directly.

The generator confines paths to the current workspace: archive inputs must be
inside `release-archives/`, and the only output is
`homebrew-tap/Formula/meshfox.rb`. Paths are resolved before access, including
symlinks; escaping either root is rejected. Run it from the workspace containing
these two directories, as the workflow does. Absolute paths within those roots
also work.

To generate a formula locally from downloaded release archives:

```sh
python3 scripts/homebrew/update_formula.py v0.26.1 release-archives homebrew-tap/Formula/meshfox.rb
python3 -m unittest discover -s scripts/homebrew -p 'test_*.py'
```
