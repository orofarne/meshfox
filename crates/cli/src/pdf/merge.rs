//! Concatenates two single-purpose PDFs (the diagram overview page and the
//! flow document pages — see the parent module's own doc comment for why
//! they're printed as two separate `Page.printToPDF` calls rather than one)
//! into a single output PDF, page order preserved. A trimmed-down version of
//! `lopdf`'s own `examples/merge.rs`: no bookmarks/table-of-contents (this
//! is always exactly two documents, page order alone is all the structure
//! `meshfox pdf` needs), just `Pages`/`Catalog` reconciliation and object
//! renumbering so the two documents' internal object ids don't collide.

use lopdf::{Document, Object, ObjectId};
use std::collections::BTreeMap;

/// `pdfs[0]`'s pages come first, then `pdfs[1]`'s, and so on.
pub fn concat(pdfs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut max_id = 1u32;
    let mut all_pages: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut all_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();

    for bytes in pdfs {
        let mut doc = Document::load_mem(bytes).map_err(|e| format!("failed to read a PDF to merge: {e}"))?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;

        for (id, object) in doc.get_pages().into_values().map(|id| (id, doc.get_object(id).unwrap().to_owned())) {
            all_pages.insert(id, object);
        }
        all_objects.extend(doc.objects);
    }

    let mut merged = Document::with_version("1.5");
    let mut catalog: Option<(ObjectId, lopdf::Dictionary)> = None;
    let mut pages: Option<(ObjectId, lopdf::Dictionary)> = None;

    for (id, object) in all_objects {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" => {
                if catalog.is_none() {
                    if let Ok(dict) = object.as_dict() {
                        catalog = Some((id, dict.clone()));
                    }
                }
            }
            b"Pages" => {
                if pages.is_none() {
                    if let Ok(dict) = object.as_dict() {
                        pages = Some((id, dict.clone()));
                    }
                }
            }
            b"Page" => {} // collected separately into `all_pages`, reattached below
            _ => {
                merged.objects.insert(id, object);
            }
        }
    }

    let (pages_id, mut pages_dict) = pages.ok_or("no PDF being merged had a Pages root")?;
    let (catalog_id, mut catalog_dict) = catalog.ok_or("no PDF being merged had a Catalog")?;

    for (id, object) in &all_pages {
        if let Object::Dictionary(dict) = object {
            let mut dict = dict.clone();
            dict.set("Parent", pages_id);
            merged.objects.insert(*id, Object::Dictionary(dict));
        }
    }
    pages_dict.set("Count", all_pages.len() as u32);
    pages_dict.set("Kids", all_pages.keys().map(|id| Object::Reference(*id)).collect::<Vec<_>>());
    merged.objects.insert(pages_id, Object::Dictionary(pages_dict));

    catalog_dict.set("Pages", pages_id);
    merged.objects.insert(catalog_id, Object::Dictionary(catalog_dict));
    merged.trailer.set("Root", catalog_id);

    merged.max_id = merged.objects.len() as u32;
    merged.renumber_objects();

    let mut out = Vec::new();
    merged.save_to(&mut out).map_err(|e| format!("failed to write the merged PDF: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{content::Content, content::Operation, dictionary, Stream};

    fn one_page_pdf(text: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" });
        let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn concatenates_page_counts_in_order() {
        let a = one_page_pdf("first");
        let b = one_page_pdf("second");
        let merged_bytes = concat(&[a, b]).unwrap();

        let merged = Document::load_mem(&merged_bytes).unwrap();
        assert_eq!(merged.get_pages().len(), 2);
        assert!(merged.extract_text(&[1]).unwrap().contains("first"));
        assert!(merged.extract_text(&[2]).unwrap().contains("second"));
    }

    #[test]
    fn a_single_pdf_still_round_trips() {
        let a = one_page_pdf("only");
        let merged_bytes = concat(&[a]).unwrap();
        let merged = Document::load_mem(&merged_bytes).unwrap();
        assert_eq!(merged.get_pages().len(), 1);
    }
}
