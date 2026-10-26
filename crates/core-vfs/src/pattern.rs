/// Case-sensitive glob matching for the Select Items dialog. `?` matches one
/// Unicode character, while `*` matches any sequence. No filesystem traversal.
pub fn matches_name_pattern(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n, mut star, mut retry) = (0, 0, None, 0);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || (pattern[p] != '*' && pattern[p] == name[n])) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if let Some(last_star) = star {
            retry += 1;
            n = retry;
            p = last_star + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_patterns_match_unicode_and_backtrack_across_stars() {
        for (pattern, name) in [
            ("*.txt", "hello.txt"),
            ("?.txt", "€.txt"),
            ("a*b*c", "a123b456c"),
            ("*", ""),
            ("*", "*rest"),
            ("**", "**rest"),
            ("a*", "a*rest"),
            ("*x*?", "xx"),
            ("**.txt", "a.txt"),
        ] {
            assert!(matches_name_pattern(pattern, name), "{pattern}: {name}");
        }
        for (pattern, name) in [("?.txt", "ab.txt"), ("*.txt", "a.TXT"), ("a*b", "abc"), ("", "x")] {
            assert!(!matches_name_pattern(pattern, name), "{pattern}: {name}");
        }
    }
}
