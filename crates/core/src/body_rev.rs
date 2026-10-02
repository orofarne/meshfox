//! A node's `bodyRev`: a short, stable fingerprint of exactly the text a
//! `PATCH /api/nodes/:id {text}` replaces (`Node::text`).
//!
//! It is computed, never stored — not in the file (an editor writing the
//! file would never bump it) and not in a side database (it would lose
//! track of every edit made outside this process). Whatever the text is
//! right now is what its revision is, so an external edit changes it
//! automatically and it survives a restart. A client sends the revision it
//! last saw along with a body replacement; the server refuses the write
//! when the text has changed since, instead of silently overwriting it.

use crate::canvas::Canvas;

/// FNV-1a, 64 bit, as 16 lowercase hex digits. Hand-rolled rather than
/// `std::hash::DefaultHasher`, whose output is explicitly not stable across
/// Rust releases, and so the revision is the same in every meshfox binary a
/// client might talk to. Not a security boundary: a revision only has to
/// differ when the text does.
pub fn body_rev(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Sets every node's `body_rev` from its current `text` — never done by
/// `mdcanvas::parse` itself, only by whichever consumer serves nodes to a
/// client (the server's canvas responses and node events), same convention
/// as `annotate_effective_colors`.
pub fn annotate_body_revs(canvas: &mut Canvas) {
    for node in &mut canvas.nodes {
        node.body_rev = Some(body_rev(&node.text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_fnv1a_64_test_vectors() {
        assert_eq!(body_rev(""), "cbf29ce484222325");
        assert_eq!(body_rev("a"), "af63dc4c8601ec8c");
        assert_eq!(body_rev("foobar"), "85944171f73967e8");
    }

    #[test]
    fn differs_when_the_text_differs_by_a_single_byte() {
        assert_ne!(body_rev("body a"), body_rev("body b"));
        assert_ne!(body_rev("body\n"), body_rev("body"));
    }

    #[test]
    fn annotating_a_canvas_gives_each_node_the_rev_of_its_own_text() {
        let mut canvas = Canvas::from_markdown(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n",
        ))
        .unwrap();
        annotate_body_revs(&mut canvas);
        let a = canvas.node("a").unwrap();
        assert_eq!(a.body_rev.as_deref(), Some(body_rev(&a.text).as_str()));
    }
}
