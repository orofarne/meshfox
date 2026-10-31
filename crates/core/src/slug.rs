//! `meshfox_model::slug` plus the filesystem-resolving entry point.

pub use meshfox_model::slug::*;

use std::path::Path;

/// Slug of `path` under `root`. Both are canonicalised first, so symlinks
/// that leave the root are [`SlugError::OutsideRoot`], not silently followed.
pub fn slug(root: &Path, path: &Path) -> Result<String, SlugError> {
    let resolve = |p: &Path| {
        p.canonicalize().map_err(|e| SlugError::Resolve {
            path: p.display().to_string(),
            message: e.to_string(),
        })
    };
    let root = resolve(root)?;
    let path = resolve(path)?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| SlugError::OutsideRoot(path.display().to_string()))?;
    slug_for_rel(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mfx-slug-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn slug_does_not_depend_on_where_the_root_is() {
        let a = tmp("root-a");
        let b = tmp("root-b");
        for root in [&a, &b] {
            fs::create_dir_all(root.join("notes")).unwrap();
            fs::write(root.join("notes/todo.canvas.md"), "# t\n").unwrap();
        }
        let sa = slug(&a, &a.join("notes/todo.canvas.md")).unwrap();
        let sb = slug(&b, &b.join("notes/todo.canvas.md")).unwrap();
        assert_eq!(sa, sb);
        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaving_the_root_is_outside_but_one_inside_is_fine() {
        use std::os::unix::fs::symlink;
        let root = tmp("symlink-root");
        let outside = tmp("symlink-outside");
        fs::write(outside.join("secret.canvas.md"), "# s\n").unwrap();
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/a.canvas.md"), "# a\n").unwrap();
        symlink(
            outside.join("secret.canvas.md"),
            root.join("leak.canvas.md"),
        )
        .unwrap();
        symlink(root.join("real/a.canvas.md"), root.join("alias.canvas.md")).unwrap();

        assert!(matches!(
            slug(&root, &root.join("leak.canvas.md")),
            Err(SlugError::OutsideRoot(_))
        ));
        // An in-root alias resolves to its target, so it shares the target's slug.
        assert_eq!(
            slug(&root, &root.join("alias.canvas.md")).unwrap(),
            slug(&root, &root.join("real/a.canvas.md")).unwrap()
        );
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn missing_file_is_a_resolve_error() {
        let root = tmp("missing");
        assert!(matches!(
            slug(&root, &root.join("nope.canvas.md")),
            Err(SlugError::Resolve { .. })
        ));
        let _ = fs::remove_dir_all(&root);
    }
}
