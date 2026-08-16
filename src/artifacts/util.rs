//! Utility helpers for artifact paths.

/// Sanitize a `--name` or goal into a directory slug.
#[must_use]
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return "task".into();
    }
    let mut slug = trimmed.chars().take(40).collect::<String>();
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "task".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_sanitizes_goal_text() {
        assert_eq!(
            slugify("Implement rate limiting!"),
            "implement-rate-limiting"
        );
        assert_eq!(slugify("  Hello   World  "), "hello-world");
        assert_eq!(slugify(""), "task");
        assert!(slugify(&"x".repeat(80)).len() <= 40);
    }
}
