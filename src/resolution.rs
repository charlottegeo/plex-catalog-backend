fn normalize_resolution_label(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub fn resolution_weight(value: &str) -> Option<u8> {
    let normalized = normalize_resolution_label(value);

    if normalized.contains("2160") || normalized.contains("4k") {
        return Some(4);
    }
    if normalized.contains("1080") {
        return Some(3);
    }
    if normalized.contains("720") {
        return Some(2);
    }
    if normalized.contains("576") || normalized.contains("480") || normalized.contains("sd") {
        return Some(1);
    }

    None
}

pub fn is_resolution_lower(actual: &str, requested: &str) -> bool {
    match (resolution_weight(actual), resolution_weight(requested)) {
        (Some(actual_weight), Some(requested_weight)) => actual_weight < requested_weight,
        _ => false,
    }
}