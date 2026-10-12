//! Pure ranking functions: straightforward to test without a desktop.

/// Prefix > word prefix > substring > ordered-character fuzzy match.
pub fn score(query: &str, name: &str, extra: &str) -> Option<i32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name == query {
        return Some(1000);
    }
    if name.starts_with(&query) {
        return Some(800);
    }
    if name.split_whitespace().any(|word| word.starts_with(&query)) {
        return Some(700);
    }
    if name.contains(&query) {
        return Some(600);
    }
    let haystack = format!("{name} {}", extra.to_lowercase());
    if query.split_whitespace().all(|term| haystack.contains(term)) {
        return Some(400);
    }
    // Unicode characters, not byte offsets: Chinese search must not panic.
    let mut needed = query.chars();
    let mut next = needed.next();
    let mut gaps = 0;
    for character in name.chars() {
        if Some(character) == next {
            next = needed.next();
            if next.is_none() {
                return Some(200 - gaps.min(150));
            }
        } else {
            gaps += 1;
        }
    }
    None
}

pub fn contains_terms(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|term| text.contains(term))
}

/// Resolve recent desktop IDs against the filtered catalog, skipping stale IDs.
pub fn recent_indices(ids: &[String], candidates: &[Option<String>], limit: usize) -> Vec<usize> {
    let mut indices = vec![];
    for id in ids {
        if indices.len() == limit {
            break;
        }
        if let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.as_ref() == Some(id))
        {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
    }
    indices
}

pub fn preview(text: &str, limit: usize) -> String {
    let prefix: String = text.chars().take(limit + 1).collect();
    let flattened = prefix.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result: String = flattened.chars().take(limit).collect();
    if text.chars().count() > limit {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking_prioritizes_exact_and_prefix_matches() {
        assert!(
            score("code", "Code", "").unwrap() > score("code", "Visual Studio Code", "").unwrap()
        );
        assert!(score("orcsl", "OrcaSlicer", "").is_some());
        assert_eq!(score("zzzz", "Files", "file manager"), None);
        assert!(score("files", "文件", "org.gnome.Nautilus Files").is_some());
    }
    #[test]
    fn unicode_and_multiline_filtering_preserves_utf8() {
        assert!(contains_terms("中文 TEST", "test\n中文内容"));
        assert_eq!(preview("剪贴板历史记录", 3), "剪贴板…");
        assert_eq!(preview("a\nb", 20), "a b");
    }
    #[test]
    fn recent_apps_skip_removed_and_duplicate_ids_before_applying_limit() {
        let ids = ["removed", "b", "b", "c", "a"].map(String::from);
        let candidates = [None, Some("a".into()), Some("b".into()), Some("c".into())];
        assert_eq!(recent_indices(&ids, &candidates, 2), vec![2, 3]);
        assert!(recent_indices(&ids, &candidates, 0).is_empty());
        assert_eq!(recent_indices(&ids, &[Some("a".into())], 5), vec![0]);
    }
}
