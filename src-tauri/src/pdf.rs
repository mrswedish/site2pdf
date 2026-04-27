use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeMap;

/// Merge multiple PDF byte slices into a single PDF.
pub fn merge_pdfs(pages: Vec<Vec<u8>>) -> Result<Vec<u8>> {
    if pages.is_empty() {
        anyhow::bail!("No pages to merge");
    }
    if pages.len() == 1 {
        return Ok(pages.into_iter().next().unwrap());
    }

    let mut docs: Vec<Document> = pages
        .iter()
        .map(|b| Document::load_mem(b).context("Failed to parse PDF"))
        .collect::<Result<_>>()?;

    // Renumber each document's objects with non-overlapping seeds
    let mut seed = 1u32;
    for doc in &mut docs {
        doc.renumber_objects_with(seed);
        seed = doc.max_id + 1;
    }

    let mut merged = Document::with_version("1.5");
    let mut all_page_ids: Vec<ObjectId> = Vec::new();
    let mut all_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();

    for doc in &docs {
        let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
        all_page_ids.extend(&page_ids);

        for (id, obj) in &doc.objects {
            match obj.type_name().unwrap_or("") {
                "Catalog" | "Pages" => {} // rebuilt below
                _ => {
                    all_objects.insert(*id, obj.clone());
                }
            }
        }

        // Keep Page objects
        for page_id in &page_ids {
            if let Ok(page_obj) = doc.get_object(*page_id) {
                all_objects.insert(*page_id, page_obj.clone());
            }
        }
    }

    // Build the Pages node
    let pages_id: ObjectId = (seed, 0);
    seed += 1;
    let catalog_id: ObjectId = (seed, 0);

    let kids: Vec<Object> = all_page_ids.iter().map(|id| Object::Reference(*id)).collect();
    let count = all_page_ids.len() as i64;

    // Update each Page's Parent reference to point to our new Pages node
    for page_id in &all_page_ids {
        if let Some(Object::Dictionary(dict)) = all_objects.get_mut(page_id) {
            dict.set("Parent", Object::Reference(pages_id));
        }
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Kids", Object::Array(kids));
    pages_dict.set("Count", Object::Integer(count));

    let mut catalog_dict = Dictionary::new();
    catalog_dict.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog_dict.set("Pages", Object::Reference(pages_id));

    merged.objects.extend(all_objects);
    merged
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));
    merged
        .objects
        .insert(catalog_id, Object::Dictionary(catalog_dict));
    merged.trailer.set("Root", Object::Reference(catalog_id));

    let mut buf = Vec::new();
    merged.save_to(&mut buf).context("Failed to serialise merged PDF")?;
    Ok(buf)
}
