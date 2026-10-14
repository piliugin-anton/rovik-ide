//! Reading the files of a GitHub release from github.com's release pages.
//!
//! The app downloads tools from official releases: rust-analyzer (lsp) and
//! the virtual files' drivers (vfs/driver.rs). It reads the release pages
//! rather than the REST API, whose 60 anonymous requests an hour per IP a
//! shared office network uses up. Every download is checked against the
//! SHA-256 GitHub publishes for it, which this finds.

/// Finds `asset`'s download path and SHA-256 in the release's asset list
/// (an HTML fragment: one `<li>` per file, with its link and digest).
pub fn parse_assets(html: &str, asset: &str) -> Option<(String, String)> {
    html.split("<li").find_map(|item| {
        let link_end = format!("/{asset}\"");
        let end = item.find(&link_end)?;
        let start = item[..end].rfind("href=\"")? + 6;
        let href = format!("{}{}", &item[start..end], &link_end[..link_end.len() - 1]);
        let at = item.find("sha256:")? + 7;
        let digest = item.get(at..at + 64)?;
        digest
            .chars()
            .all(|c| c.is_ascii_hexdigit())
            .then(|| (href, digest.to_ascii_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_asset_and_its_checksum() {
        let html = r#"<ul><li class="Box-row"><a href="/rust-lang/rust-analyzer/releases/download/2026-09-28/rust-analyzer-x86_64-unknown-linux-gnu.gz" rel="nofollow">x</a>
            <span>sha256:23F711D86B5F826E0000000000000000000000000000000000000000000000AB</span></li>
            <li class="Box-row"><a href="/rust-lang/rust-analyzer/releases/download/2026-09-28/rust-analyzer-x86_64-unknown-linux-musl.gz">y</a>
            <span>sha256:b9a0edbda7a7d9ef000000000000000000000000000000000000000000000000</span></li></ul>"#;
        let (href, digest) =
            parse_assets(html, "rust-analyzer-x86_64-unknown-linux-musl.gz").unwrap();
        assert_eq!(href, "/rust-lang/rust-analyzer/releases/download/2026-09-28/rust-analyzer-x86_64-unknown-linux-musl.gz");
        assert!(digest.starts_with("b9a0edbda7a7d9ef"));
        let (_, gnu) = parse_assets(html, "rust-analyzer-x86_64-unknown-linux-gnu.gz").unwrap();
        assert!(gnu.starts_with("23f711d86b5f826e"), "lowercased");
        assert!(parse_assets(html, "rust-analyzer-aarch64-apple-darwin.gz").is_none());
    }
}
