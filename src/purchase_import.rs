//! Shared purchase payload validation for desktop HTTP and browser imports.
use crate::model::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
pub(crate) fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{k} must be a string"))
}
pub(crate) fn quantity(v: &Value, default: i64) -> Result<i64> {
    let n = match v.get("quantity") {
        Some(v) => v
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Quantity must be a whole number"))?,
        None => default,
    };
    ensure!(n > 0, "Quantity must be positive");
    Ok(n)
}
pub(crate) fn patch_part(p: &Part, body: &Value) -> Result<Part> {
    let object = body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object"))?;
    let allowed = [
        "number",
        "manufacturer",
        "spn",
        "supplier",
        "lcsc",
        "description",
        "package",
        "value",
        "tolerance",
        "stock",
        "storage",
        "datasheet",
        "labels",
        "customFields",
        "notes",
    ];
    let mut value = serde_json::to_value(p)?;
    for (k, v) in object {
        ensure!(allowed.contains(&k.as_str()), "Unsupported part field: {k}");
        value[k] = v.clone();
    }
    if let Some(lcsc) = body.get("lcsc") {
        ensure!(
            body.get("spn").is_none() && body.get("supplier").is_none(),
            "Do not combine lcsc and spn/supplier"
        );
        value["spn"] = lcsc.clone();
        value["supplier"] = json!(if lcsc.as_str() == Some("") {
            ""
        } else {
            "LCSC"
        });
        value.as_object_mut().unwrap().remove("lcsc");
    }
    let result: Part = serde_json::from_value(value)?;
    result.validate()?;
    Ok(result)
}
pub fn purchase(body: &Value, old: Option<&Purchase>, image: Option<&Value>) -> Result<Purchase> {
    ensure!(
        body.get("imageBase64").is_none() || image.is_some(),
        "Photo upload requires managed attachment storage"
    );
    let mut p = old.cloned().unwrap_or(Purchase {
        part: Part {
            stock: 1,
            ..Default::default()
        },
        assignments: vec![],
    });
    if let Some(part) = body.get("part") {
        p.part = patch_part(&p.part, part)?;
    }
    if body.get("quantity").is_some() {
        p.part.stock = quantity(body, p.part.stock)?;
    }
    if let Some(assignments) = body.get("assignments") {
        p.assignments = assignments
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Assignments must be an array"))?
            .iter()
            .map(|a| {
                Ok(Assignment {
                    id: id(),
                    project: string(a, "project")?.into(),
                    quantity: quantity(a, 1)?,
                    designators: a
                        .get("designators")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                })
            })
            .collect::<Result<_>>()?;
    }
    if let Some(image) = image {
        p.part.image = Some(image.clone());
    }
    p.part.validate()?;
    Ok(p)
}

/// Stable request receipts let a sender safely retry after a lost response.
pub fn existing(data: &Inventory, body: &Value) -> Result<Option<Purchase>> {
    let Some(request) = body.get("requestID") else {
        return Ok(None);
    };
    let request = request
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| anyhow::anyhow!("Invalid requestID"))?;
    Ok(data
        .purchases
        .iter()
        .find(|p| {
            p.part
                .extra
                .get("supplierImportRequestID")
                .and_then(Value::as_str)
                == Some(request)
                || p.part
                    .extra
                    .get("supplierImportRequestIDs")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(request)))
        })
        .cloned())
}
pub fn record_request(purchase: &mut Purchase, body: &Value) {
    if let Some(request) = body.get("requestID") {
        let ids = purchase
            .part
            .extra
            .entry("supplierImportRequestIDs".into())
            .or_insert_with(|| json!([]));
        if let Some(ids) = ids.as_array_mut()
            && !ids.contains(request)
        {
            ids.push(request.clone());
        }
        purchase
            .part
            .extra
            .insert("supplierImportRequestID".into(), request.clone());
    }
}

/// Shared POST behavior for HTTP and the browser bridge. PATCH still replaces quantity.
pub(crate) fn import(
    data: &mut Inventory,
    body: &Value,
    image: Option<&Value>,
) -> Result<(bool, Purchase)> {
    if let Some(existing) = existing(data, body)? {
        return Ok((false, existing));
    }
    let incoming = purchase(body, None, image)?;
    let index = data.purchases.iter().position(|p| {
        (!incoming.part.number.trim().is_empty()
            && key(&p.part.number) == key(&incoming.part.number))
            || (!incoming.part.spn.trim().is_empty() && key(&p.part.spn) == key(&incoming.part.spn))
    });
    let mut merged = if let Some(index) = index {
        let old = &data.purchases[index];
        let mut merged = purchase(body, Some(old), image)?;
        merged.part.stock = old
            .part
            .stock
            .checked_add(incoming.part.stock)
            .ok_or_else(|| anyhow::anyhow!("Purchase quantity is too large"))?;
        // Keep project reservations when adding another delivery of the same part.
        merged.assignments = old.assignments.clone();
        merged.assignments.extend(incoming.assignments.clone());
        if let Some(previous) = old.part.extra.get("supplierImportRequestID") {
            record_request(&mut merged, &json!({"requestID":previous}));
        }
        merged
    } else {
        incoming
    };
    record_request(&mut merged, body);
    for assignment in &merged.assignments {
        data.ensure_project(&assignment.project)?;
    }
    if let Some(index) = index {
        data.purchases[index] = merged.clone();
    } else {
        data.purchases.push(merged.clone());
    }
    Ok((index.is_none(), merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_number_or_spn_refreshes_fields_image_and_preserves_retry_receipts() {
        let mut data = Inventory::default();
        let first = json!({"part":{"number":"ABC", "spn":"C123", "supplier":"LCSC", "manufacturer":"Old", "description":"Original"}, "quantity":2, "requestID":"first"});
        let (_, original) = import(&mut data, &first, None).unwrap();
        let image = json!({"path":"new-image.png"});
        let second = json!({"part":{"number":"abc", "spn":"C456", "supplier":"LCSC", "manufacturer":"New"}, "quantity":3, "requestID":"second"});
        let (created, updated) = import(&mut data, &second, Some(&image)).unwrap();
        assert!(!created);
        assert_eq!(updated.part.id, original.part.id);
        assert_eq!(updated.part.stock, 5);
        assert_eq!(updated.part.manufacturer, "New");
        assert_eq!(updated.part.description, "Original");
        assert_eq!(updated.part.image, Some(image));
        let third = json!({"part":{"number":"Other", "spn":"c456", "supplier":"LCSC", "description":"Updated"}, "quantity":4, "requestID":"third"});
        import(&mut data, &third, None).unwrap();
        assert_eq!(data.purchases.len(), 1);
        assert_eq!(data.purchases[0].part.stock, 9);
        assert_eq!(data.purchases[0].part.description, "Updated");
        import(&mut data, &first, None).unwrap();
        import(&mut data, &second, None).unwrap();
        assert_eq!(data.purchases[0].part.stock, 9);
    }

    #[test]
    fn blank_identifiers_do_not_merge_unrelated_purchases() {
        let mut data = Inventory::default();
        for number in ["ABC", "DEF"] {
            import(
                &mut data,
                &json!({"part":{"number":number}, "quantity":1}),
                None,
            )
            .unwrap();
        }
        assert_eq!(data.purchases.len(), 2);
    }
}
