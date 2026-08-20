//! Dynamic resolution of `include` nodes (see `crate::canvas::NodeType`).
//!
//! This never touches disk beyond *reading* include targets: `resolve`
//! takes a parsed `Canvas` and returns a new one with every `include` node
//! expanded in memory. `check` never calls this — it works on a single
//! file's raw text directly, and sees a bare `[label](target)` link, same
//! as `file`/`link`. Consumers that want the fully composed view — the
//! server (before serving `GET /api/canvas`), `run`/the TUI (via
//! `crate::locate::locate_node`, to address and run a block living inside
//! an `include` target) — call it directly or through that.
//!
//! Two kinds of target, told apart the same way `meshfox`'s own
//! auto-discovery does (filename ends in `.canvas.md`, or a plain `.md`
//! file opens with the `<!-- meshfox:canvas -->` marker):
//!
//! - a **canvas**: parsed and spliced in as real children, ids namespaced
//!   `{include_id}/{original_id}` to avoid collisions, every level shifted
//!   down by the include node's own level. The include node itself becomes
//!   a `group` (empty body, its box derived from its new children, same as
//!   any other group).
//! - **plain Markdown**: not structurally meaningful to meshfox, so it
//!   becomes the include node's own body verbatim — except every heading
//!   in it is shifted down (`mdcanvas::shift_headings`) by the include
//!   node's level, so e.g. its own top-level `#` doesn't read as a second
//!   document root. The include node becomes a `text` node.
//!
//! Included files can themselves declare includes; `resolve` expands those
//! too, tracking the chain of files currently being expanded to reject a
//! cycle (A includes B includes A) with a clear error instead of recursing
//! forever.

use crate::canvas::{Canvas, ExtraEdge, Node, NodeType};
use crate::mdcanvas;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IncludeError {
    #[error("include node {0:?} has no target")]
    MissingTarget(String),
    #[error("include target {0} not found: {1}")]
    NotFound(PathBuf, #[source] std::io::Error),
    #[error("include cycle: {0} is already being included by one of its own includes")]
    Cycle(PathBuf),
    #[error("include target {0} failed to parse as a canvas: {1}")]
    TargetParse(PathBuf, mdcanvas::ParseError),
}

/// Work item: expand the include node `id` (declared in a file at `dir`,
/// having reached it via `ancestors` — for cycle detection).
struct Job {
    id: String,
    dir: PathBuf,
    ancestors: Vec<PathBuf>,
}

/// Returns a new `Canvas` with every `include` node (transitively) expanded
/// — see the module docs. `base_path` is the file `canvas` was parsed from,
/// used to resolve top-level include targets relative to its directory.
pub fn resolve(canvas: &Canvas, base_path: &Path) -> Result<Canvas, IncludeError> {
    let mut nodes = canvas.nodes.clone();
    let base_dir = base_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let base_ancestors: Vec<PathBuf> = base_path.canonicalize().into_iter().collect();

    let mut queue: Vec<Job> = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Include)
        .map(|n| Job {
            id: n.id.clone(),
            dir: base_dir.clone(),
            ancestors: base_ancestors.clone(),
        })
        .collect();

    while let Some(job) = queue.pop() {
        let idx = nodes
            .iter()
            .position(|n| n.id == job.id)
            .expect("job id always refers to a node just inserted into `nodes`");
        let level = nodes[idx].level;
        let target = nodes[idx]
            .target
            .clone()
            .ok_or_else(|| IncludeError::MissingTarget(job.id.clone()))?;
        let target_path = job.dir.join(&target);

        let canon = target_path
            .canonicalize()
            .map_err(|e| IncludeError::NotFound(target_path.clone(), e))?;
        if job.ancestors.contains(&canon) {
            return Err(IncludeError::Cycle(target_path));
        }
        let contents = std::fs::read_to_string(&target_path)
            .map_err(|e| IncludeError::NotFound(target_path.clone(), e))?;

        let mut ancestors = job.ancestors.clone();
        ancestors.push(canon.clone());
        let dir = target_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // `canon` is the canonicalized target *file*, so its parent is the
        // canonicalized target *directory* — used (rather than the
        // possibly-relative `dir`) so a relative asset reference (an
        // `![](...)` image, a plain link) in the spliced text can be told
        // apart from one that's relative to the including document's own
        // directory instead (see `Node::asset_base`, and the server's
        // `/api/include-asset` handler that resolves against it).
        let asset_base = canon.parent().map(|p| p.to_string_lossy().into_owned());

        let is_canvas = target_path.to_string_lossy().ends_with(".canvas.md")
            || mdcanvas::has_marker(&contents);

        if is_canvas {
            let included = mdcanvas::parse(&contents)
                .map_err(|e| IncludeError::TargetParse(target_path.clone(), e))?;
            let prefix = format!("{}/", job.id);
            let origin_path = canon.to_string_lossy().into_owned();
            let mut spliced: Vec<Node> = included
                .nodes
                .into_iter()
                .map(|mut n| {
                    n.parent = Some(match n.parent {
                        None => job.id.clone(),
                        Some(p) => format!("{prefix}{p}"),
                    });
                    // Record where this node's content really lives (its
                    // id *in that file*, before namespacing below renames
                    // `n.id` itself) — a consumer that wants to write an
                    // edit back to the right file (the server's mutating
                    // endpoints) uses this rather than trying to reverse
                    // the namespacing itself.
                    n.origin_path = Some(origin_path.clone());
                    n.origin_id = Some(n.id.clone());
                    n.id = format!("{prefix}{}", n.id);
                    n.extra_parents = n
                        .extra_parents
                        .iter()
                        .map(|e| ExtraEdge {
                            from: format!("{prefix}{}", e.from),
                            ..e.clone()
                        })
                        .collect();
                    n.level = (n.level + level).min(6);
                    n.asset_base = asset_base.clone();
                    n
                })
                .collect();

            for n in &spliced {
                if n.node_type == NodeType::Include {
                    queue.push(Job {
                        id: n.id.clone(),
                        dir: dir.clone(),
                        ancestors: ancestors.clone(),
                    });
                }
            }

            nodes[idx].node_type = NodeType::Group;
            nodes[idx].target = None;
            nodes[idx].text = String::new();
            nodes.append(&mut spliced);
        } else {
            nodes[idx].node_type = NodeType::Text;
            nodes[idx].target = None;
            nodes[idx].text = mdcanvas::shift_headings(&contents, level);
            nodes[idx].asset_base = asset_base;
            nodes[idx].plain_markdown_include = true;
        }
    }

    Ok(Canvas {
        nodes,
        options: Vec::new(),
    })
}

/// One `include` node reachable from `canvas` (transitively — an include
/// target that itself declares includes contributes its own entries too),
/// resolved to the physical file it points at, but without splicing
/// anything in — a read-only companion to `resolve` for a consumer that
/// wants to know *what's there* (the web UI's Source-mode file picker)
/// rather than the fully composed document.
#[derive(Debug, Clone)]
pub struct IncludeInfo {
    /// This include node's own id, in whichever document declares it —
    /// namespaced the same way a spliced node's `id` would be
    /// (`{outer_include_id}/{...}`) when this include is itself nested
    /// inside another one.
    pub node_id: String,
    pub title: String,
    /// The literal link target as written (e.g. `./LICENSE.canvas.md`).
    pub target: String,
    /// Absolute path it resolves to.
    pub path: PathBuf,
    /// Nesting depth: 0 for an include declared directly in `canvas`
    /// itself, 1 for one declared inside a depth-0 include's own target,
    /// and so on.
    pub depth: u32,
    /// Whether this resolved cleanly enough to have been walked for
    /// further nested includes (a canvas target that read and parsed) —
    /// `false` for a plain-Markdown target (nothing to descend into
    /// anyway), one that failed to read/parse, or one that would form a
    /// cycle. Still reported either way, so a broken or cyclic include
    /// doesn't just silently vanish from the picker.
    pub is_canvas: bool,
}

struct ListJob {
    id: String,
    title: String,
    target: String,
    dir: PathBuf,
    depth: u32,
    ancestors: Vec<PathBuf>,
}

/// Lists every include reachable from `canvas` (see `IncludeInfo`),
/// without ever splicing content in — `base_path` is the file `canvas` was
/// parsed from, same as `resolve`.
pub fn list_includes(canvas: &Canvas, base_path: &Path) -> Vec<IncludeInfo> {
    let base_dir = base_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let base_ancestors: Vec<PathBuf> = base_path.canonicalize().into_iter().collect();

    let mut queue: Vec<ListJob> = canvas
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Include)
        .filter_map(|n| {
            n.target.clone().map(|target| ListJob {
                id: n.id.clone(),
                title: n.title.clone(),
                target,
                dir: base_dir.clone(),
                depth: 0,
                ancestors: base_ancestors.clone(),
            })
        })
        .collect();

    let mut out = Vec::new();
    while let Some(job) = queue.pop() {
        let target_path = job.dir.join(&job.target);
        let Ok(canon) = target_path.canonicalize() else {
            out.push(IncludeInfo {
                node_id: job.id,
                title: job.title,
                target: job.target,
                path: target_path,
                depth: job.depth,
                is_canvas: false,
            });
            continue;
        };
        // A cycle (or the primary document including itself) — report the
        // entry so it's still visible, but don't walk into it again.
        if job.ancestors.contains(&canon) {
            out.push(IncludeInfo {
                node_id: job.id,
                title: job.title,
                target: job.target,
                path: canon,
                depth: job.depth,
                is_canvas: false,
            });
            continue;
        }
        let contents = std::fs::read_to_string(&canon).ok();
        let is_canvas = target_path.to_string_lossy().ends_with(".canvas.md")
            || contents.as_deref().is_some_and(mdcanvas::has_marker);

        if is_canvas {
            if let Some(parsed) = contents.as_deref().and_then(|c| mdcanvas::parse(c).ok()) {
                let prefix = format!("{}/", job.id);
                let dir = canon.parent().map(Path::to_path_buf).unwrap_or_default();
                let mut ancestors = job.ancestors.clone();
                ancestors.push(canon.clone());
                for n in &parsed.nodes {
                    if n.node_type != NodeType::Include {
                        continue;
                    }
                    if let Some(target) = &n.target {
                        queue.push(ListJob {
                            id: format!("{prefix}{}", n.id),
                            title: n.title.clone(),
                            target: target.clone(),
                            dir: dir.clone(),
                            depth: job.depth + 1,
                            ancestors: ancestors.clone(),
                        });
                    }
                }
            }
        }

        out.push(IncludeInfo {
            node_id: job.id,
            title: job.title,
            target: job.target,
            path: canon,
            depth: job.depth,
            is_canvas,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn includes_plain_markdown_as_shifted_body_text() {
        let tmp = std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let _target = write(
            &tmp,
            "spec.md",
            "# Spec Title\n\nSome body.\n\n## Sub\nmore\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Spec\n<!-- meshfox:node id=\"spec\" type=\"include\" -->\n\n[spec](./spec.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let resolved = resolve(&canvas, &base).unwrap();

        let spec = resolved.node("spec").unwrap();
        assert_eq!(spec.node_type, NodeType::Text);
        assert!(spec.target.is_none());
        // Marks this node's body as belonging to the target file, not its
        // own — the signal a client (e.g. the web UI) uses to steer a
        // would-be per-node body editor toward Source mode instead.
        assert!(spec.plain_markdown_include);
        // Spec's own H1 (level 1) becomes level 1+2=3 (spec node is level 2).
        assert!(spec.text.contains("### Spec Title"));
        assert!(spec.text.contains("#### Sub"));
        // No new nodes: plain markdown has no meshfox structure of its own.
        assert_eq!(resolved.nodes.len(), 2);
        // A relative asset (e.g. `![](fig.png)`) in the target's body
        // resolves against the target's own directory, not the including
        // document's — recorded here so a consumer (the server) can serve
        // it correctly instead of 404ing against the wrong directory.
        assert_eq!(
            spec.asset_base.as_deref(),
            Some(tmp.canonicalize().unwrap().to_str().unwrap())
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn includes_canvas_as_namespaced_children() {
        let tmp =
            std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id() + 1));
        fs::create_dir_all(&tmp).unwrap();
        let _target = write(
            &tmp,
            "child.canvas.md",
            "<!-- meshfox:canvas -->\n# Child Root\n<!-- meshfox:node id=\"root\" -->\n\nintro\n\n## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\nbody\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let resolved = resolve(&canvas, &base).unwrap();

        let include_node = resolved.node("child").unwrap();
        assert_eq!(include_node.node_type, NodeType::Group);
        assert!(!include_node.plain_markdown_include);

        let spliced_root = resolved.node("child/root").unwrap();
        assert_eq!(spliced_root.parent.as_deref(), Some("child"));
        assert_eq!(spliced_root.level, 2 + 1); // child node is level 2
                                               // A canvas-include descendant has its own real on-disk identity
                                               // (origin_path/origin_id) and is safely per-node-editable — not
                                               // the same "can't write this back" case plain-Markdown is.
        assert!(!spliced_root.plain_markdown_include);
        assert_eq!(
            spliced_root.asset_base.as_deref(),
            Some(tmp.canonicalize().unwrap().to_str().unwrap())
        );

        let leaf = resolved.node("child/leaf").unwrap();
        assert_eq!(leaf.parent.as_deref(), Some("child/root"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn detects_include_cycles() {
        let tmp =
            std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id() + 2));
        fs::create_dir_all(&tmp).unwrap();
        write(
            &tmp,
            "a.canvas.md",
            "<!-- meshfox:canvas -->\n# A\n<!-- meshfox:node -->\n\n## B\n<!-- meshfox:node id=\"b\" type=\"include\" -->\n\n[b](./b.canvas.md)\n",
        );
        let base = write(
            &tmp,
            "b.canvas.md",
            "<!-- meshfox:canvas -->\n# B\n<!-- meshfox:node -->\n\n## A\n<!-- meshfox:node id=\"a\" type=\"include\" -->\n\n[a](./a.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let err = resolve(&canvas, &base).unwrap_err();
        assert!(
            matches!(err, IncludeError::Cycle(_)),
            "expected Cycle, got {err:?}"
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_includes_reports_a_top_level_canvas_include() {
        let tmp =
            std::env::temp_dir().join(format!("meshfox-list-includes-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        write(
            &tmp,
            "child.canvas.md",
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\nbody\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let includes = list_includes(&canvas, &base);

        assert_eq!(includes.len(), 1);
        assert_eq!(includes[0].node_id, "child");
        assert_eq!(includes[0].title, "Child");
        assert_eq!(includes[0].depth, 0);
        assert!(includes[0].is_canvas);
        assert_eq!(
            includes[0].path,
            tmp.join("child.canvas.md").canonicalize().unwrap()
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_includes_reports_a_nested_include_at_depth_one_with_a_composed_id() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-list-includes-test-{}",
            std::process::id() + 1
        ));
        fs::create_dir_all(&tmp).unwrap();
        write(
            &tmp,
            "grandchild.canvas.md",
            "<!-- meshfox:canvas -->\n# Grandchild\n<!-- meshfox:node -->\n\nbody\n",
        );
        write(
            &tmp,
            "child.canvas.md",
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n## Grandchild\n<!-- meshfox:node id=\"nested\" type=\"include\" -->\n\n[grandchild](./grandchild.canvas.md)\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let mut includes = list_includes(&canvas, &base);
        includes.sort_by_key(|i| i.depth);

        assert_eq!(includes.len(), 2);
        assert_eq!(includes[0].node_id, "child");
        assert_eq!(includes[0].depth, 0);
        assert_eq!(includes[1].node_id, "child/nested");
        assert_eq!(includes[1].depth, 1);
        assert_eq!(
            includes[1].path,
            tmp.join("grandchild.canvas.md").canonicalize().unwrap()
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_includes_reports_a_plain_markdown_target_as_not_a_canvas() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-list-includes-test-{}",
            std::process::id() + 2
        ));
        fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "notes.md", "# Notes\n\nprose\n");
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[notes](./notes.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let includes = list_includes(&canvas, &base);

        assert_eq!(includes.len(), 1);
        assert!(!includes[0].is_canvas);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_includes_does_not_loop_forever_on_a_cycle() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-list-includes-test-{}",
            std::process::id() + 3
        ));
        fs::create_dir_all(&tmp).unwrap();
        write(
            &tmp,
            "a.canvas.md",
            "<!-- meshfox:canvas -->\n# A\n<!-- meshfox:node -->\n\n## B\n<!-- meshfox:node id=\"b\" type=\"include\" -->\n\n[b](./b.canvas.md)\n",
        );
        let base = write(
            &tmp,
            "b.canvas.md",
            "<!-- meshfox:canvas -->\n# B\n<!-- meshfox:node -->\n\n## A\n<!-- meshfox:node id=\"a\" type=\"include\" -->\n\n[a](./a.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        // The real assertion is just that this returns at all rather than
        // hanging: `a` (a.canvas.md, a genuinely new file) is walked and
        // reported, but its own nested include `a/b` — pointing back at
        // `b.canvas.md`, already an ancestor — is reported as the cycle's
        // dead end instead of being walked into again.
        let mut includes = list_includes(&canvas, &base);
        includes.sort_by_key(|i| i.depth);
        assert_eq!(includes.len(), 2);
        assert_eq!(includes[0].node_id, "a");
        assert!(includes[0].is_canvas);
        assert_eq!(includes[1].node_id, "a/b");
        assert!(!includes[1].is_canvas);

        fs::remove_dir_all(&tmp).ok();
    }
}
