//! Raster previews of imported SVGs. No script execution or external resources.
use prism::hardware::{Image, Pixel};
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

const MAX_SIDE: u32 = 1600;
const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 32;
const MAX_FILTER_PRIMITIVES: usize = 32;
const MAX_INTERMEDIATE_PIXELS: u64 = 32_000_000;
const COMPLEX_SVG: &str =
    "This SVG is too complex to preview safely. Export it to view the original in another app.";

// Bound the XML and reference expansion before usvg creates its recursive tree.
// In particular, a small sequence of <use> definitions can expand exponentially.
fn checked_xml(source: &str) -> Result<usvg::roxmltree::Document<'_>, String> {
    use usvg::roxmltree::{Document, Node, ParsingOptions};
    // SVG exporters commonly include an external SVG 1.1 DOCTYPE. roxmltree
    // never fetches it; reject entity declarations before enabling that syntax.
    if source.contains("<!ENTITY") {
        return Err(COMPLEX_SVG.into());
    }
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: true,
            nodes_limit: MAX_NODES as u32,
            ..ParsingOptions::default()
        },
    )
    .map_err(|_| {
        "This SVG could not be read. It may be damaged, unsupported, or too complex.".to_string()
    })?;
    let mut ids = HashMap::new();
    for node in document.descendants() {
        if let Some(id) = node.attribute("id") {
            // Match usvg's first-definition rule for malformed duplicate IDs.
            ids.entry(id).or_insert(node);
        }
    }
    fn visit<'a, 'input>(
        node: Node<'a, 'input>,
        ids: &HashMap<&'a str, Node<'a, 'input>>,
        depth: usize,
        nodes: &mut usize,
        filters: &mut usize,
    ) -> Result<(), String> {
        *nodes += 1;
        if *nodes > MAX_NODES || depth > MAX_DEPTH {
            return Err(COMPLEX_SVG.into());
        }
        if node.is_element() && node.tag_name().name().starts_with("fe") {
            *filters += 1;
            if *filters > MAX_FILTER_PRIMITIVES {
                return Err(COMPLEX_SVG.into());
            }
        }
        if node.has_tag_name("use") {
            let href = node
                .attribute(("http://www.w3.org/1999/xlink", "href"))
                .or_else(|| node.attribute("href"));
            if let Some(target) = href
                .and_then(|href| href.trim().strip_prefix('#'))
                .and_then(|id| ids.get(id))
            {
                // Cycles reach the same depth limit before conversion.
                visit(*target, ids, depth + 1, nodes, filters)?;
            }
        }
        for child in node.children() {
            visit(child, ids, depth + 1, nodes, filters)?;
        }
        Ok(())
    }
    visit(document.root(), &ids, 0, &mut 0, &mut 0)?;
    Ok(document)
}

#[derive(Default)]
struct RenderBudget {
    nodes: usize,
    pixels: u64,
    segments: usize,
}

impl RenderBudget {
    fn pixels(&mut self, width: f32, height: f32, copies: usize) -> Result<u64, String> {
        if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
            return Err(COMPLEX_SVG.into());
        }
        let pixels = f64::from(width.ceil()) * f64::from(height.ceil());
        if pixels > MAX_INTERMEDIATE_PIXELS as f64 {
            return Err(COMPLEX_SVG.into());
        }
        let pixels = pixels as u64;
        self.pixels = self
            .pixels
            .saturating_add(pixels.saturating_mul(copies as u64));
        if self.pixels > MAX_INTERMEDIATE_PIXELS {
            return Err(COMPLEX_SVG.into());
        }
        Ok(pixels)
    }

    fn group(
        &mut self,
        group: &usvg::Group,
        transform: tiny_skia::Transform,
        depth: usize,
    ) -> Result<(), String> {
        self.nodes += group.children().len() + 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(COMPLEX_SVG.into());
        }
        let transform = transform.pre_concat(group.transform());
        // resvg retains a layer for every nested isolated group, and every
        // filter result. A bounded output canvas alone does not bound these.
        let bounds = group
            .layer_bounding_box()
            .transform(transform)
            .ok_or(COMPLEX_SVG)?;
        let width = bounds.width().min(MAX_SIDE as f32 * 5.0) + 4.0;
        let height = bounds.height().min(MAX_SIDE as f32 * 5.0) + 4.0;
        if group.should_isolate() {
            let primitives: usize = group
                .filters()
                .iter()
                .map(|filter| filter.primitives().len())
                .sum();
            if primitives > MAX_FILTER_PRIMITIVES {
                return Err(COMPLEX_SVG.into());
            }
            self.pixels(width, height, 1 + primitives * 3)?;
        }
        let mut clip = group.clip_path();
        while let Some(current) = clip {
            self.pixels(width, height, 2)?;
            self.group(
                current.root(),
                transform.pre_concat(current.transform()),
                depth + 1,
            )?;
            clip = current.clip_path();
        }
        let mut mask = group.mask();
        while let Some(current) = mask {
            self.pixels(width, height, 3)?;
            self.group(current.root(), transform, depth + 1)?;
            mask = current.mask();
        }
        for filter in group.filters() {
            for primitive in filter.primitives() {
                if let usvg::filter::Kind::Image(image) = primitive.kind() {
                    self.group(image.root(), transform, depth + 1)?;
                }
            }
        }
        for node in group.children() {
            match node {
                usvg::Node::Group(child) => self.group(child, transform, depth + 1)?,
                usvg::Node::Text(text) => self.group(text.flattened(), transform, depth + 1)?,
                usvg::Node::Path(path) => {
                    self.segments = self.segments.saturating_add(path.data().segments().count());
                    if self.segments > 100_000 {
                        return Err(COMPLEX_SVG.into());
                    }
                    for paint in [
                        path.fill().map(|fill| fill.paint()),
                        path.stroke().map(|stroke| stroke.paint()),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if let usvg::Paint::Pattern(pattern) = paint {
                            // Unlike ordinary group layers, pattern tiles are
                            // not clipped to the output canvas by resvg.
                            let (sx, sy) = transform.pre_concat(pattern.transform()).get_scale();
                            self.pixels(
                                pattern.rect().width() * sx,
                                pattern.rect().height() * sy,
                                1,
                            )?;
                            self.group(
                                pattern.root(),
                                tiny_skia::Transform::from_scale(sx, sy),
                                depth + 1,
                            )?;
                        }
                    }
                }
                usvg::Node::Image(_) => {}
            }
        }
        Ok(())
    }
}

fn add_fallback_font(database: &mut usvg::fontdb::Database, bytes: &[u8]) {
    use usvg::fontdb::{Family, Query, Source};
    let serif_missing = database
        .query(&Query {
            families: &[Family::Serif],
            ..Query::default()
        })
        .is_none();
    let sans_missing = database
        .query(&Query {
            families: &[Family::SansSerif],
            ..Query::default()
        })
        .is_none();
    let loaded = database.load_font_source(Source::Binary(Arc::new(bytes.to_vec())));
    let family = loaded
        .first()
        .and_then(|id| database.face(*id))
        .and_then(|face| face.families.first())
        .map(|family| family.0.clone());
    if let Some(family) = family {
        if serif_missing {
            database.set_serif_family(family.clone());
        }
        if sans_missing {
            database.set_sans_serif_family(family);
        }
    }
}

#[cfg(test)]
fn render(bytes: &[u8]) -> Result<Image, String> {
    render_with_font(bytes, None)
}

pub fn render_with_font(bytes: &[u8], fallback_font: Option<&[u8]>) -> Result<Image, String> {
    if bytes.len() > crate::storage::MAX_FILE_SIZE {
        return Err("This SVG is too large to preview.".into());
    }
    let source = std::str::from_utf8(bytes)
        .map_err(|_| "This SVG is not readable UTF-8 text.".to_string())?;
    let document = checked_xml(source)?;
    // A shared system font database avoids rescanning fonts for every preview.
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let mut fonts = FONTS
        .get_or_init(|| {
            let mut database = usvg::fontdb::Database::new();
            database.load_system_fonts();
            database.set_serif_family("Times New Roman");
            database.set_sans_serif_family("Arial");
            Arc::new(database)
        })
        .clone();
    if let Some(bytes) = fallback_font {
        // iOS does not expose system fonts through fontdb's filesystem scan.
        // CHK already carries an app font, so use that for missing families.
        add_fallback_font(Arc::make_mut(&mut fonts), bytes);
    }
    let blocked = Arc::new(AtomicBool::new(false));
    let external = blocked.clone();
    let embedded = blocked.clone();
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    let options = usvg::Options {
        font_family: fonts.family_name(&usvg::fontdb::Family::Serif).to_string(),
        fontdb: fonts,
        image_href_resolver: usvg::ImageHrefResolver {
            // usvg's default string resolver reads local files. Imported SVGs
            // must not gain access to unrelated device files or network URLs.
            resolve_string: Box::new(move |_, _| {
                external.store(true, Ordering::Relaxed);
                None
            }),
            resolve_data: Box::new(move |mime, data, options| {
                // Keep embedded raster images bounded before resvg decodes
                // them. Nested SVG data URIs are rejected rather than allowing
                // unbounded recursive document parsing.
                let dimensions = chk::image::io::Reader::new(std::io::Cursor::new(data.as_slice()))
                    .with_guessed_format()
                    .ok()
                    .and_then(|reader| reader.into_dimensions().ok());
                let safe = dimensions.is_some_and(|(w, h)| {
                    w > 0
                        && h > 0
                        && w <= 8192
                        && h <= 8192
                        && u64::from(w) * u64::from(h) <= 16_000_000
                });
                if data.len() > crate::storage::MAX_FILE_SIZE || !safe {
                    embedded.store(true, Ordering::Relaxed);
                    None
                } else {
                    default_data(mime, data, options)
                }
            }),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_xmltree(&document, &options)
        .map_err(|_| "This SVG could not be read. It may be damaged or unsupported.".to_string())?;
    if blocked.load(Ordering::Relaxed) {
        return Err("This SVG uses linked or unsupported embedded images. Export it to view the original in another app.".into());
    }
    let size = tree.size();
    let scale = MAX_SIDE as f32 / size.width().max(size.height());
    if !scale.is_finite() || scale <= 0.0 {
        return Err("This SVG has invalid dimensions.".into());
    }
    let width = (size.width() * scale).round().clamp(1.0, MAX_SIDE as f32) as u32;
    let height = (size.height() * scale).round().clamp(1.0, MAX_SIDE as f32) as u32;
    RenderBudget::default().group(
        tree.root(),
        tiny_skia::Transform::from_scale(scale, scale),
        0,
    )?;
    let mut pixels =
        tiny_skia::Pixmap::new(width, height).ok_or("This SVG could not be rendered.")?;
    pixels.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    // The white canvas makes these premultiplied pixels fully opaque RGBA.
    let rgba = pixels
        .data()
        .chunks_exact(4)
        .map(|pixel| Pixel {
            red: pixel[0],
            green: pixel[1],
            blue: pixel[2],
            alpha: pixel[3],
        })
        .collect();
    Ok(Image::new((width, height), rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradients_text_and_transparency_render_to_bounded_rgba() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><defs><linearGradient id="g"><stop stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect width="200" height="80" fill="url(#g)"/><text x="12" y="55" font-family="Arial" font-size="30" fill="white">Fig</text></svg>"##;
        let image = render(svg).unwrap();
        assert_eq!(image.dimensions, (1600, 800));
        let rgba: &[u8] = image.as_ref();
        let left = &rgba[(100 * 1600 + 40) * 4..][..4];
        let right = &rgba[(100 * 1600 + 1560) * 4..][..4];
        assert!(left[0] > 200 && left[2] < 30);
        assert!(right[2] > 200 && right[0] < 30);
        assert_eq!(&rgba[(750 * 1600 + 20) * 4..][..4], &[255; 4]);
    }

    #[test]
    fn invalid_svg_and_external_images_are_not_previewed() {
        assert!(render(b"not svg").is_err());
        for href in [
            "file:///etc/passwd",
            "/tmp/private.png",
            "https://example.com/image.png",
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><image href="{href}" width="100" height="100"/></svg>"#
            );
            assert!(render(svg.as_bytes()).unwrap_err().contains("linked"));
        }
    }

    #[test]
    fn huge_declared_dimensions_are_scaled_before_allocating_pixels() {
        let image = render(br#"<svg xmlns="http://www.w3.org/2000/svg" width="1000000" height="500000"><rect width="1000000" height="500000" fill="red"/></svg>"#).unwrap();
        assert_eq!(image.dimensions, (1600, 800));
    }

    #[test]
    fn nested_layers_and_filter_chains_are_rejected_before_rasterization() {
        let layers = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">{}<rect width="100" height="100"/>{}</svg>"#,
            r#"<g opacity="0.9">"#.repeat(20),
            "</g>".repeat(20),
        );
        // XML depth is allowed; the intermediate pixel budget rejects this.
        assert!(checked_xml(&layers).is_ok());
        assert!(
            render(layers.as_bytes())
                .unwrap_err()
                .contains("too complex")
        );
        let filters = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs><filter id="f">{}</filter></defs><rect width="100" height="100" filter="url(#f)"/></svg>"#,
            r#"<feFlood flood-color="red"/>"#.repeat(MAX_FILTER_PRIMITIVES + 1),
        );
        assert!(checked_xml(&filters).unwrap_err().contains("too complex"));
    }

    #[test]
    fn xml_depth_nodes_entities_and_expanding_references_are_bounded() {
        let deep = format!(
            "<svg>{}<path/>{}</svg>",
            "<g>".repeat(MAX_DEPTH),
            "</g>".repeat(MAX_DEPTH)
        );
        assert!(checked_xml(&deep).is_err());
        let wide = format!("<svg>{}</svg>", "<path/>".repeat(MAX_NODES));
        assert!(checked_xml(&wide).is_err());
        assert!(
            checked_xml(r#"<!DOCTYPE svg [<!ENTITY text "expanded">]><svg>&text;</svg>"#).is_err()
        );
        let mut definitions = String::from(r#"<g id="n0"><path/></g>"#);
        for index in 1..15 {
            definitions.push_str(&format!(
                r##"<g id="n{index}"><use href="#n{}"/><use href="#n{}"/></g>"##,
                index - 1,
                index - 1
            ));
        }
        let expanded = format!(r##"<svg><defs>{definitions}</defs><use href="#n14"/></svg>"##);
        assert!(checked_xml(&expanded).unwrap_err().contains("too complex"));
        assert!(checked_xml(r##"<svg><g id="loop"><use href="#loop"/></g></svg>"##).is_err());
    }

    #[test]
    fn enormous_pattern_tiles_are_rejected_but_ordinary_effects_render() {
        let enormous = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs><pattern id="p" patternUnits="userSpaceOnUse" width="1000000" height="1000000"><rect width="10" height="10" fill="red"/></pattern></defs><rect width="100" height="100" fill="url(#p)"/></svg>"##;
        assert!(render(enormous).unwrap_err().contains("too complex"));
        let ordinary = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><defs><pattern id="p" patternUnits="userSpaceOnUse" width="20" height="20"><rect width="10" height="10" fill="red"/></pattern><filter id="f"><feGaussianBlur stdDeviation="1"/></filter></defs><g opacity="0.8"><rect x="10" y="10" width="40" height="40" fill="url(#p)" filter="url(#f)"/></g></svg>"##;
        assert_eq!(render(ordinary).unwrap().dimensions, (1600, 800));
    }

    #[test]
    fn app_font_supplies_text_when_system_fonts_are_unavailable() {
        use usvg::fontdb::{Database, Family, Query};
        let theme = chk::ChkTheme::default().build::<chk::PelicanUi>(chk::Assets::new(Vec::new()));
        let font = theme
            .fonts()
            .get_font("FontStyle::Text")
            .expect("CHK bundles its text font");
        let mut database = Database::new();
        add_fallback_font(&mut database, font.bytes());
        for family in [Family::Serif, Family::SansSerif] {
            assert!(
                database
                    .query(&Query {
                        families: &[family],
                        ..Query::default()
                    })
                    .is_some()
            );
        }
        let options = usvg::Options {
            font_family: database.family_name(&Family::Serif).to_string(),
            fontdb: Arc::new(database),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40"><text x="5" y="30" font-size="24">Fig</text></svg>"#, &options).unwrap();
        assert!(tree.has_text_nodes());
        let mut pixmap = tiny_skia::Pixmap::new(100, 40).unwrap();
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        assert!(pixmap.pixels().iter().any(|pixel| pixel.alpha() > 0));
    }

    #[test]
    fn exporter_doctype_is_supported_without_external_fetching() {
        let svg = br#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd"><svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="red"/></svg>"#;
        assert_eq!(render(svg).unwrap().dimensions, (1600, 800));
    }
}
