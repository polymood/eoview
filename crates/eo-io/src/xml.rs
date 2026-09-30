//! Small XML search for product metadata files. It finds elements by local name (any namespace
//! prefix). It does not validate the XML.

/// Elements with local name `name`: (attributes text, inner text), in document order.
pub fn elems<'a>(xml: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = vec![];
    let mut p = 0;
    while let Some((s, ts)) = open_tag(xml, name, p) {
        let Some(e) = xml[ts..].find('>').map(|i| ts + i) else { break };
        let attrs = xml[ts..e].trim_end_matches('/');
        if xml[..e].ends_with('/') {
            out.push((attrs, ""));
            p = e + 1;
            continue;
        }
        // Close tag, with nested elements of the same name.
        let (mut depth, mut q) = (1, e + 1);
        let mut end = xml.len();
        while depth > 0 {
            let next_open = open_tag(xml, name, q).map(|(s, _)| s);
            let Some(c) = close_tag(xml, name, q) else { break };
            match next_open {
                Some(o) if o < c.0 => {
                    depth += 1;
                    q = o + 1;
                }
                _ => {
                    depth -= 1;
                    q = c.1;
                    end = c.0;
                }
            }
        }
        out.push((attrs, &xml[e + 1..end]));
        p = if depth == 0 { q } else { e + 1 };
        let _ = s;
    }
    out
}

/// Position of the next start tag `<prefix:name` from `from`, and the position after the name.
fn open_tag(xml: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    let mut p = from;
    while let Some(i) = xml[p..].find('<') {
        let s = p + i;
        let rest = &xml[s + 1..];
        let local = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/').map(|n| &rest[..n]).unwrap_or(rest);
        let bare = local.rsplit(':').next().unwrap_or(local);
        if bare == name && !local.starts_with('/') && !local.starts_with('?') && !local.starts_with('!') {
            return Some((s, s + 1 + local.len()));
        }
        p = s + 1;
    }
    None
}

/// Position of the next close tag `</prefix:name>` from `from`, and the position after it.
fn close_tag(xml: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    let mut p = from;
    while let Some(i) = xml[p..].find("</") {
        let s = p + i;
        let e = s + xml[s..].find('>')?;
        let local = xml[s + 2..e].trim();
        if local.rsplit(':').next() == Some(name) {
            return Some((s, e + 1));
        }
        p = e;
    }
    None
}

/// Value of attribute `key` in an attributes text.
pub fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let mut p = 0;
    while let Some(i) = attrs[p..].find(key) {
        let s = p + i;
        let before = attrs[..s].chars().last();
        let after = attrs[s + key.len()..].trim_start();
        if before.is_none_or(|c| c.is_whitespace() || c == ':')
            && let Some(v) = after.strip_prefix('=')
        {
            let v = v.trim_start();
            let q = v.chars().next()?;
            return v[1..].split(q).next();
        }
        p = s + key.len();
    }
    None
}

/// Inner text of the first element `name`, without XML entities, trimmed.
pub fn text(xml: &str, name: &str) -> Option<String> {
    elems(xml, name).first().map(|e| unescape(e.1.trim()))
}

pub fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_and_prefixed() {
        let x = r#"<n1:Root><a id="1"><a>in</a></a><b k='v' x="y"/><a id="2">t &amp; u</a></n1:Root>"#;
        let a = elems(x, "a");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].1, "<a>in</a>");
        assert_eq!(attr(a[1].0, "id"), Some("2"));
        assert_eq!(text(x, "Root").map(|s| s.len()).unwrap() > 10, true);
        assert_eq!(attr(elems(x, "b")[0].0, "k"), Some("v"));
        assert_eq!(unescape(a[1].1), "t & u");
    }
}
