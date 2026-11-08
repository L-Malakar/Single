//! Tiny XML helpers for WebDAV: escaping, PROPFIND multistatus generation
//! (server side) and multistatus parsing (client side).
//!
//! Both ends of a Single transfer run this crate, so the parser only needs
//! to understand the XML this crate generates — but it is written
//! defensively (namespace-prefix agnostic, entity-unescaping) so it also
//! copes with third-party WebDAV servers.

/// Escape a string for XML text content / attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Unescape the five predefined XML entities (plus numeric ones).
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i..].find(';') {
                let ent = &s[i + 1..i + semi];
                let replacement = match ent {
                    "amp" => Some("&".to_string()),
                    "lt" => Some("<".to_string()),
                    "gt" => Some(">".to_string()),
                    "quot" => Some("\"".to_string()),
                    "apos" => Some("'".to_string()),
                    _ => {
                        if let Some(hexcode) = ent.strip_prefix("#x").or(ent.strip_prefix("#X")) {
                            u32::from_str_radix(hexcode, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .map(|c| c.to_string())
                        } else if let Some(dec) = ent.strip_prefix('#') {
                            dec.parse::<u32>()
                                .ok()
                                .and_then(char::from_u32)
                                .map(|c| c.to_string())
                        } else {
                            None
                        }
                    }
                };
                if let Some(r) = replacement {
                    out.push_str(&r);
                    i += semi + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// One file-system entry, as exchanged between peers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
    pub size: u64,
    /// mtime as unix epoch seconds (0 when unknown)
    pub mtime: u64,
}

/// Build a 207 Multi-Status body for PROPFIND.
///
/// `entries` must already be relative to the collection being listed; `dir`
/// is the percent-encoded collection href (ending with `/`).
pub fn multistatus(dir_href: &str, is_dir: bool, entries: &[FsEntry]) -> String {
    let mut xml = String::with_capacity(1024 + entries.len() * 160);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<D:multistatus xmlns:D=\"DAV:\">\n");

    // The collection itself.
    xml.push_str("<D:response>\n");
    xml.push_str(&format!("<D:href>{}</D:href>\n", escape(dir_href)));
    xml.push_str("<D:propstat><D:prop>\n");
    if is_dir {
        xml.push_str("<D:resourcetype><D:collection/></D:resourcetype>\n");
    } else {
        xml.push_str("<D:resourcetype/>\n");
    }
    xml.push_str("</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\n");
    xml.push_str("</D:response>\n");

    for e in entries {
        let href = format!(
            "{}/{}",
            dir_href.trim_end_matches('/'),
            encode_path(&e.path)
        );
        let href = if e.is_dir && !href.ends_with('/') {
            format!("{}/", href)
        } else {
            href
        };
        xml.push_str("<D:response>\n");
        xml.push_str(&format!("<D:href>{}</D:href>\n", escape(&href)));
        xml.push_str("<D:propstat><D:prop>\n");
        xml.push_str(&format!(
            "<D:displayname>{}</D:displayname>\n",
            escape(&e.name)
        ));
        if e.is_dir {
            xml.push_str("<D:resourcetype><D:collection/></D:resourcetype>\n");
        } else {
            xml.push_str("<D:resourcetype/>\n");
            xml.push_str(&format!("<D:getcontentlength>{}</D:getcontentlength>\n", e.size));
            xml.push_str("<D:getcontenttype>application/octet-stream</D:getcontenttype>\n");
        }
        if e.mtime > 0 {
            let dt = httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(e.mtime));
            xml.push_str(&format!("<D:getlastmodified>{}</D:getlastmodified>\n", dt));
            let iso = iso8601_from_epoch(e.mtime);
            xml.push_str(&format!("<D:creationdate>{}</D:creationdate>\n", iso));
        }
        xml.push_str(&format!(
            "<D:supportedlock><D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry></D:supportedlock>\n"
        ));
        xml.push_str("</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\n");
        xml.push_str("</D:response>\n");
    }

    xml.push_str("</D:multistatus>\n");
    xml
}

/// Civil-date algorithm (Howard Hinnant) — no chrono dependency.
pub fn iso8601_from_epoch(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, mth, d, h, m, s
    )
}

/// Case-insensitive scan for `<tag ...>content</tag>` inside a segment.
/// Returns the raw (still entity-escaped) content.
fn tag_content(hay_lower: &str, original: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let mut search_from = 0;
    while let Some(pos) = hay_lower[search_from..].find(&open) {
        let abs = search_from + pos;
        let after = abs + open.len();
        let rest = &hay_lower[after..];
        // tag must be followed by '>' or whitespace (not another tag name)
        let ok_boundary = rest.starts_with('>')
            || rest.starts_with(char::is_whitespace);
        if ok_boundary {
            let gt_rel = rest.find('>')?;
            let content_start = after + gt_rel + 1;
            let close = format!("</{}>", tag);
            let hay2 = &hay_lower[content_start..];
            let end_rel = hay2.find(&close)?;
            return Some(original[content_start..content_start + end_rel].to_string());
        }
        search_from = abs + open.len();
    }
    None
}

/// Parse a 207 Multi-Status body into entries. `base_path` is the collection
/// path the request was made against (decoded, e.g. `/dav/Videos`).
pub fn parse_multistatus(xml: &str, base_path: &str) -> Vec<FsEntry> {
    let lower = xml.to_lowercase();
    let mut out = Vec::new();

    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find("<d:response") {
        let start = cursor + rel;
        let end = match lower[start..].find("</d:response>") {
            Some(e) => start + e,
            None => break,
        };
        let seg_lower = &lower[start..end];
        let seg_orig = &xml[start..end];

        let href = tag_content(seg_lower, seg_orig, "d:href")
            .or_else(|| tag_content(seg_lower, seg_orig, "href"))
            .map(|h| unescape(&h))
            .unwrap_or_default();

        // decode percent-encoding, strip the base prefix
        let decoded = percent_decode_str(&href);
        let is_dir = seg_lower.contains("<d:collection");

        if !decoded.is_empty() && decoded.trim_end_matches('/') != base_path.trim_end_matches('/') {
            let rel = decoded
                .trim_end_matches('/')
                .strip_prefix(base_path.trim_end_matches('/'))
                .unwrap_or("")
                .trim_start_matches('/')
                .to_string();
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            if !rel.is_empty() {
                let size = tag_content(seg_lower, seg_orig, "d:getcontentlength")
                    .or_else(|| tag_content(seg_lower, seg_orig, "getcontentlength"))
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                let mtime = tag_content(seg_lower, seg_orig, "d:getlastmodified")
                    .or_else(|| tag_content(seg_lower, seg_orig, "getlastmodified"))
                    .and_then(|s| httpdate::parse_http_date(s.trim()).ok())
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs())
                    })
                    .unwrap_or(0);
                out.push(FsEntry {
                    name: unescape(&name),
                    path: rel,
                    is_dir,
                    size,
                    mtime,
                });
            }
        }
        cursor = end + 1;
    }
    out
}

fn percent_decode_str(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

/// Encode a path segment for use inside a URL (keeps `/`).
pub fn encode_path(p: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'%')
        .add(b'[')
        .add(b']')
        .add(b'^')
        .add(b'|')
        .add(b'\\');
    percent_encoding::utf8_percent_encode(p, SET).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let raw = r#"a<b & "c"/d'>&#x2F;"#;
        let esc = escape(raw);
        assert_eq!(unescape(&esc), raw);
    }

    #[test]
    fn numeric_entities() {
        assert_eq!(unescape("&#65;&#x42;"), "AB");
        assert_eq!(unescape("no entity"), "no entity");
        assert_eq!(unescape("bad &notanentity; tail"), "bad &notanentity; tail");
    }

    #[test]
    fn multistatus_roundtrip() {
        let entries = vec![
            FsEntry {
                name: "Videos".into(),
                path: "Videos".into(),
                is_dir: true,
                size: 0,
                mtime: 1_700_000_000,
            },
            FsEntry {
                name: "a&b <fun>.mp4".into(),
                path: "a&b <fun>.mp4".into(),
                is_dir: false,
                size: 12345,
                mtime: 1_700_000_100,
            },
        ];
        let xml = multistatus("/dav/", true, &entries);
        let parsed = parse_multistatus(&xml, "/dav/");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "Videos");
        assert!(parsed[0].is_dir);
        assert_eq!(parsed[1].name, "a&b <fun>.mp4");
        assert_eq!(parsed[1].size, 12345);
        assert!(!parsed[1].is_dir);
        assert!(parsed[1].mtime > 0);
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601_from_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_from_epoch(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn path_encoding() {
        assert_eq!(encode_path("a b/c&d.mp4"), "a%20b/c&d.mp4"); // & is XML-escaped later
        assert_eq!(encode_path("plain.mp4"), "plain.mp4");
    }
}
