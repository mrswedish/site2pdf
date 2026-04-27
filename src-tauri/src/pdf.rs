use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};

/// Merge multiple single-page PDF byte slices into one document.
pub fn merge_pdfs(pages: Vec<Vec<u8>>) -> Result<Vec<u8>> {
    if pages.is_empty() {
        anyhow::bail!("No pages to merge");
    }
    if pages.len() == 1 {
        return Ok(pages.into_iter().next().unwrap());
    }

    let mut docs: Vec<Document> = pages
        .iter()
        .map(|b| Document::load_mem(b).context("Failed to parse PDF page"))
        .collect::<Result<_>>()?;

    // Renumber every document's object IDs with non-overlapping seeds
    // so we can safely combine them into one object table.
    let mut seed = 1u32;
    for doc in &mut docs {
        doc.renumber_objects_with(seed);
        seed = doc.max_id + 1;
    }

    // Build merged document. Set max_id so new_object_id() is safe.
    let mut merged = Document::with_version("1.5");
    merged.max_id = seed;

    let mut page_ids: Vec<ObjectId> = Vec::new();

    for doc in docs {
        for (id, obj) in doc.objects {
            match obj.type_name().unwrap_or("") {
                // Catalog and Pages are rebuilt below; skip source copies.
                "Catalog" | "Pages" => {}
                "Page" => {
                    page_ids.push(id);
                    merged.objects.insert(id, obj);
                }
                _ => {
                    merged.objects.insert(id, obj);
                }
            }
        }
    }

    // Allocate IDs for the unified Pages node and Catalog.
    let pages_id = merged.new_object_id();
    let catalog_id = merged.new_object_id();

    // Every Page must point to our new Pages node as its Parent.
    for page_id in &page_ids {
        if let Some(obj) = merged.objects.get_mut(page_id) {
            if let Ok(dict) = obj.as_dict_mut() {
                dict.set("Parent", Object::Reference(pages_id));
            }
        }
    }

    let kids: Vec<Object> = page_ids.iter().map(|id| Object::Reference(*id)).collect();
    let count = page_ids.len() as i64;

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Kids", Object::Array(kids));
    pages_dict.set("Count", Object::Integer(count));

    let mut catalog_dict = Dictionary::new();
    catalog_dict.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog_dict.set("Pages", Object::Reference(pages_id));

    merged.objects.insert(pages_id, Object::Dictionary(pages_dict));
    merged.objects.insert(catalog_id, Object::Dictionary(catalog_dict));
    merged.trailer.set("Root", Object::Reference(catalog_id));

    let mut buf = Vec::new();
    merged
        .save_to(&mut buf)
        .context("Failed to serialise merged PDF")?;
    Ok(buf)
}
