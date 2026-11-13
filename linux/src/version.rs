//! Driver version helpers. Package versions look like `1:550.54.14-2~deb12u1`,
//! `570.144-1` or `3:570.144-1.fc41`; we only care about the upstream part.

use std::cmp::Ordering;

/// Strip epoch and packaging suffix: "1:550.54.14-2" -> "550.54.14".
pub fn upstream(raw: &str) -> Option<String> {
    let s = raw.trim();
    let s = match s.find(':') {
        Some(i) => &s[i + 1..],
        None => s,
    };
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            out.push(c);
        } else {
            break;
        }
    }
    let out = out.trim_matches('.').to_string();
    if out.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        Some(out)
    } else {
        None
    }
}

fn segments(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

/// Numeric, segment-wise comparison ("550.9" < "550.54.14"; missing segments count as 0).
pub fn compare(a: &str, b: &str) -> Ordering {
    let (x, y) = (segments(a), segments(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        match p.cmp(&q) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_packaging() {
        assert_eq!(upstream("1:550.54.14-2~deb12u1").as_deref(), Some("550.54.14"));
        assert_eq!(upstream("570.144-1").as_deref(), Some("570.144"));
        assert_eq!(upstream("3:570.144-1.fc41").as_deref(), Some("570.144"));
        assert_eq!(upstream("535.183.01-0ubuntu0.24.04.1").as_deref(), Some("535.183.01"));
        assert_eq!(upstream("(none)"), None);
        assert_eq!(upstream(""), None);
    }

    #[test]
    fn compares_numerically() {
        assert_eq!(compare("550.9", "550.54.14"), Ordering::Less);
        assert_eq!(compare("570.144", "570.144.00"), Ordering::Equal);
        assert_eq!(compare("570.144.03", "550.163.01"), Ordering::Greater);
        assert_eq!(compare("535.183.01", "535.183.06"), Ordering::Less);
    }
}
