use crate::{
    crawl,
    extract::{self, ExtractError, ExtractOptions},
};

#[test]
fn html_extract_excludes_script_and_navigation() {
    let html = br#"<html><head><title>Article</title><script>SECRET_SCRIPT</script></head><body><nav>SECRET_NAV</nav><main><h1>Portable headline</h1><p>Visible article content.</p></main></body></html>"#;
    let out = extract::extract(
        html,
        "text/html",
        "https://example.test/a",
        &ExtractOptions::default(),
    )
    .unwrap();
    assert!(out.markdown.contains("Portable headline"));
    assert!(out.markdown.contains("Visible article content"));
    assert!(!out.markdown.contains("SECRET_SCRIPT"));
    assert!(!out.markdown.contains("SECRET_NAV"));
}

#[test]
fn html_extract_selector_missing_is_an_error() {
    let opts = ExtractOptions {
        selector: Some(".missing".into()),
        ..Default::default()
    };
    let result = extract::extract(
        b"<html><body><p>Body</p></body></html>",
        "text/html",
        "https://example.test",
        &opts,
    );
    assert!(matches!(result, Err(ExtractError::SelectorNoMatch { .. })));
}

#[test]
fn html_probe_reports_match_and_no_match_without_page_dump() {
    let body = format!(
        "<html><body><article><h1>Search</h1><p>Rust ownership keeps data safe.</p><p>{}</p></article></body></html>",
        "unrelated reference material ".repeat(80)
    );
    let hit = ExtractOptions {
        must_contain: Some("ownership".into()),
        ..Default::default()
    };
    let miss = ExtractOptions {
        must_contain: Some("missing phrase".into()),
        ..Default::default()
    };
    let yes = extract::extract(body.as_bytes(), "text/html", "https://example.test", &hit).unwrap();
    let no = extract::extract(body.as_bytes(), "text/html", "https://example.test", &miss).unwrap();
    assert!(yes.markdown.to_lowercase().contains("match"));
    assert!(yes.markdown.len() < body.len());
    assert!(no.markdown.contains("NO MATCH"));
}

#[test]
fn unicode_pagination_never_splits_a_codepoint() {
    let text = "aé🙂z";
    let (first, next) = extract::paginate_public(text, 2, 4);
    assert_eq!(first, "🙂");
    assert_eq!(next, Some(7));
}

#[test]
fn robots_longest_match_and_sitemap_inventory_are_portable() {
    let robots = crawl::Robots::parse(
        "User-agent: *\nDisallow: /private\nAllow: /private/open\nSitemap: /sitemap.xml",
        "https://example.test",
    );
    assert!(!robots.allowed("/private/plan"));
    assert!(robots.allowed("/private/open/page"));
    assert_eq!(robots.sitemaps, ["https://example.test/sitemap.xml"]);

    let mut entries = Vec::new();
    crawl::parse_sitemap(
        "<urlset><url><loc><![CDATA[https://example.test/a?x=1&amp;y=2]]></loc><lastmod>2026-01-02</lastmod><priority>0.8</priority></url></urlset>",
        &mut entries,
        10,
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].loc, "https://example.test/a?x=1&y=2");
    assert_eq!(entries[0].priority, Some(0.8));
}

#[test]
fn pdf_text_is_extracted_and_invalid_or_empty_pdfs_error() {
    let pdf = tiny_pdf("Portable text extraction works.");
    let out = extract::extract(
        &pdf,
        "application/pdf",
        "https://example.test/file.pdf",
        &ExtractOptions::default(),
    )
    .unwrap();
    assert!(out.markdown.contains("Portable text extraction works"));
    assert!(
        extract::extract(
            b"%PDF-invalid",
            "application/pdf",
            "https://example.test/bad.pdf",
            &ExtractOptions::default()
        )
        .is_err()
    );

    // Valid one-page PDF with an embedded raster image and no text layer.
    let scanned = tiny_scanned_pdf();
    let result = extract::extract(
        &scanned,
        "application/pdf",
        "https://example.test/scan.pdf",
        &ExtractOptions::default(),
    );
    assert!(matches!(result, Err(ExtractError::Pdf(_))));
}

fn tiny_pdf(text: &str) -> Vec<u8> {
    let content = if text.is_empty() {
        String::new()
    } else {
        format!(
            "BT /F1 12 Tf 72 720 Td ({}) Tj ET",
            text.replace('(', "\\(").replace(')', "\\)")
        )
    };
    let stream = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        content
    );
    pdf_from_objects(vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream.into_bytes(),
    ])
}

fn tiny_scanned_pdf() -> Vec<u8> {
    let content = "q 100 0 0 100 72 620 cm /Im1 Do Q";
    let page_content = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        content
    );
    let image = b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n\xff\x00\x00\nendstream";
    pdf_from_objects(vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 6 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        page_content.into_bytes(),
        image.to_vec(),
    ])
}

fn pdf_from_objects(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn focus_omits_irrelevant_sections_and_shells_remain_thin() {
    let html = r#"<html><body><main><h1>Guide</h1><h2>Orbit mechanics</h2><p>Orbital mechanics explains how spacecraft move around a planet through gravity and velocity.</p><p>Kepler describes the path and period of each orbit around the central body.</p><h2>Garden tools</h2><p>Pruning shears cut small branches cleanly and safely in a garden.</p><p>A watering can carries water to seedlings and flower beds.</p></main></body></html>"#;
    let opts = ExtractOptions {
        focus: Some("orbital mechanics".into()),
        ..Default::default()
    };
    let out = extract::extract(
        html.as_bytes(),
        "text/html",
        "https://example.test/guide",
        &opts,
    )
    .unwrap();
    assert!(out.focus_omitted);
    assert!(out.markdown.contains("Orbital mechanics"));
    assert!(!out.markdown.contains("Pruning shears"));

    let shell = format!(
        "<html><body><div>{}</div></body></html>",
        " ".repeat(22_000)
    );
    let out = extract::extract(
        shell.as_bytes(),
        "text/html",
        "https://example.test/app",
        &ExtractOptions::default(),
    )
    .unwrap();
    assert!(out.thin);
}

#[test]
fn text_wall_is_reported_as_a_wall_error() {
    let body = b"Access denied\nBlocked by network security. Please sign in to continue.";
    let result = extract::extract(
        body,
        "text/plain",
        "https://example.test",
        &ExtractOptions::default(),
    );
    assert!(matches!(result, Err(ExtractError::Wall(_))));
}

#[test]
fn html_challenge_interstitial_is_not_extracted_as_content() {
    let body = b"<html><head><title>Just a moment...</title></head><body><h1>Just a moment...</h1><noscript>Enable JavaScript and cookies to continue</noscript><p>Performing security verification</p><p>This website uses a security service to protect against malicious bots. This page is displayed while the website verifies you are not a bot.</p><script src=\"https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1\"></script></body></html>";
    let result = extract::extract(
        body,
        "text/html",
        "https://example.test",
        &ExtractOptions::default(),
    );
    assert!(matches!(result, Err(ExtractError::Wall(_))));
}

#[test]
fn robots_named_group_overrides_wildcard_and_merges_matching_groups() {
    let rules = crawl::Robots::parse(
        "User-agent: *\nDisallow: /\nCrawl-delay: 60\nUser-agent: gEnTsWeBrUnTiMe\nDisallow: /private\nCrawl-delay: 2\nUser-agent: GentsWebRuntime\nAllow: /private/public\nCrawl-delay: 5\nSitemap: /sitemap.xml",
        "https://example.test",
    );
    assert!(rules.allowed("/"));
    assert!(!rules.allowed("/private/document"));
    assert!(rules.allowed("/private/public/document"));
    assert_eq!(rules.crawl_delay, Some(5.0));
    assert_eq!(rules.sitemaps, ["https://example.test/sitemap.xml"]);
}

#[test]
fn robots_consecutive_agents_share_rules_but_unrelated_groups_do_not() {
    let rules = crawl::Robots::parse(
        "User-agent: GentsWebRuntime\nUser-agent: AnotherBot\nDisallow: /shared\nUser-agent: Donsetch\nDisallow: /unrelated\nCrawl-delay: 10",
        "https://example.test",
    );
    assert!(!rules.allowed("/shared"));
    assert!(rules.allowed("/unrelated"));
    assert_eq!(rules.crawl_delay, None);
}

#[test]
fn robots_empty_named_group_still_replaces_wildcard() {
    let rules = crawl::Robots::parse(
        "User-agent: *\nDisallow: /\nUser-agent: GentsWebRuntime\nDisallow:",
        "https://example.test",
    );
    assert!(rules.allowed("/"));
}
