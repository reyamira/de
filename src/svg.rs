//! Static, resource-free rendering for untrusted SVG files and Markdown images.

use resvg::{tiny_skia, usvg};
use std::sync::{Arc, LazyLock};

pub(crate) const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_NODES: u32 = 4096;
const MAX_DEPTH: usize = 64;
const MAX_EXPANDED_NODES: usize = 16_384;
const MAX_EXPANDED_BYTES: usize = 4 * 1024 * 1024;
const MAX_RASTER_WORK_PIXELS: u64 = 64 * 1024 * 1024;

pub(crate) fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = usvg::fontdb::Database::new();
        for bytes in [
            fmd_font::bundled::PLEX_REGULAR,
            fmd_font::bundled::PLEX_BOLD,
            fmd_font::bundled::CM_TYPEWRITER,
            fmd_font::bundled::noto_sans_cjk_sc(),
            fmd_font::bundled::NOTO_EMOJI_CURATED,
        ] {
            db.load_font_data(bytes.to_vec());
        }
        db.set_serif_family("IBM Plex Sans");
        db.set_sans_serif_family("IBM Plex Sans");
        db.set_monospace_family("CMU Typewriter Text");
        Arc::new(db)
    });
    Arc::clone(&FONTS)
}

pub(crate) struct Svg {
    tree: usvg::Tree,
    pub(crate) intrinsic_pixels: u64,
    width: u32,
    height: u32,
    scale: f32,
}

impl Svg {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        if source.len() as u64 > MAX_SOURCE_BYTES {
            return Err("SVG exceeds the 1 MiB source limit.".into());
        }
        let xml = usvg::roxmltree::Document::parse_with_options(
            source,
            usvg::roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_NODES,
            },
        )
        .map_err(|error| format!("Cannot parse SVG: {error}"))?;
        let mut styles = 0;
        let mut ids = std::collections::HashMap::new();
        let mut filters = 0;
        let mut primitives = 0;
        for node in xml.descendants().filter(|node| node.is_element()) {
            if node.ancestors().count() > MAX_DEPTH {
                return Err("SVG exceeds the nesting limit.".into());
            }
            if let Some(id) = node.attribute("id")
                && ids.insert(id, node).is_some()
            {
                return Err("SVG element IDs must be unique.".into());
            }
            let tag = node.tag_name().name();
            if matches!(
                tag,
                "script"
                    | "foreignObject"
                    | "animate"
                    | "animateMotion"
                    | "animateTransform"
                    | "set"
            ) {
                return Err(
                    "SVG scripts, foreign objects, and animation are not supported.".into(),
                );
            }
            if tag == "filter" {
                filters += 1;
                if filters > 16 || node.children().filter(|child| child.is_element()).count() > 16 {
                    return Err("SVG exceeds the filter count or primitive limit.".into());
                }
            }
            if tag.starts_with("fe") {
                primitives += 1;
                if primitives > 64 {
                    return Err("SVG exceeds the filter primitive limit.".into());
                }
                if !matches!(
                    tag,
                    "feBlend"
                        | "feColorMatrix"
                        | "feComponentTransfer"
                        | "feComposite"
                        | "feDropShadow"
                        | "feFlood"
                        | "feGaussianBlur"
                        | "feMerge"
                        | "feMergeNode"
                        | "feOffset"
                        | "feFuncR"
                        | "feFuncG"
                        | "feFuncB"
                        | "feFuncA"
                ) {
                    return Err("SVG uses an unsupported filter primitive.".into());
                }
            }
            if tag == "style" {
                styles += node.text().unwrap_or("").len();
                if styles > 16 * 1024 {
                    return Err("SVG exceeds the 16 KiB stylesheet limit.".into());
                }
                check_resource_urls(node.text().unwrap_or(""))?;
            }
            for attr in node.attributes() {
                check_resource_urls(attr.value())?;
            }
            // Use the renderer's IRI parser, including its fragment whitespace rules.
            for attr in node.attributes().filter(|attr| attr.name() == "href") {
                if matches!(tag, "image" | "feImage")
                    || svgtypes::IRI::from_str(attr.value()).is_err()
                {
                    return Err(
                        "SVG image and external resource references are not supported.".into(),
                    );
                }
            }
        }
        // Bound internal <use> expansion before usvg clones referenced subtrees.
        // Count definitions too: an unused recursive definition is rejected conservatively.
        fn visit<'a, 'input>(
            node: usvg::roxmltree::Node<'a, 'input>,
            ids: &std::collections::HashMap<&'a str, usvg::roxmltree::Node<'a, 'input>>,
            depth: usize,
            remaining: &mut usize,
            bytes: &mut usize,
        ) -> Result<(), String> {
            if depth > MAX_DEPTH || *remaining == 0 {
                return Err("SVG exceeds the internal reference complexity limit.".into());
            }
            *remaining -= 1;
            let size = node
                .attributes()
                .map(|attr| attr.value().len())
                .sum::<usize>()
                + node
                    .children()
                    .filter(|child| child.is_text())
                    .map(|child| child.text().unwrap_or("").len())
                    .sum::<usize>();
            *bytes = bytes
                .checked_sub(size)
                .ok_or("SVG exceeds the expanded source limit.")?;
            if node.tag_name().name() == "use" {
                for attr in node.attributes().filter(|attr| attr.name() == "href") {
                    if let Some(target) = svgtypes::IRI::from_str(attr.value())
                        .ok()
                        .and_then(|iri| ids.get(iri.0))
                    {
                        visit(*target, ids, depth + 1, remaining, bytes)?;
                    }
                }
            }
            for child in node.children().filter(|node| node.is_element()) {
                visit(child, ids, depth + 1, remaining, bytes)?;
            }
            Ok(())
        }
        let mut remaining = MAX_EXPANDED_NODES;
        let mut bytes = MAX_EXPANDED_BYTES;
        visit(xml.root_element(), &ids, 0, &mut remaining, &mut bytes)?;
        let options = usvg::Options {
            font_family: "IBM Plex Sans".into(),
            fontdb: fonts(),
            // Do not share the trusted FMD resolver or permit nested SVG/data images.
            resources_dir: None,
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_data: Box::new(|_, _, _| None),
                resolve_string: Box::new(|_, _| None),
            },
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_xmltree(&xml, &options)
            .map_err(|error| format!("Cannot render SVG: {error}"))?;
        let size = tree.size();
        let width = size.width().ceil() as u64;
        let height = size.height().ceil() as u64;
        let intrinsic_pixels = width.saturating_mul(height);
        if width == 0
            || height == 0
            || width > 16_384
            || height > 16_384
            || intrinsic_pixels > 16 * 1024 * 1024
        {
            return Err("SVG dimensions exceed the preview limit.".into());
        }
        let scale = (2048.0 / size.width()).min(2048.0 / size.height()).min(1.0);
        check_render_budget(&tree, scale)?;
        Ok(Self {
            width: (size.width() * scale).ceil().min(2048.0) as u32,
            height: (size.height() * scale).ceil().min(2048.0) as u32,
            tree,
            scale,
            intrinsic_pixels,
        })
    }

    pub(crate) fn output_pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    pub(crate) fn render(&self) -> Result<Vec<u8>, String> {
        let mut pixmap = tiny_skia::Pixmap::new(self.width, self.height)
            .ok_or("Cannot allocate SVG preview.")?;
        resvg::render(
            &self.tree,
            tiny_skia::Transform::from_scale(self.scale, self.scale),
            &mut pixmap.as_mut(),
        );
        pixmap
            .encode_png()
            .map_err(|error| format!("Cannot encode SVG preview: {error}"))
    }
}

fn check_resource_urls(mut value: &str) -> Result<(), String> {
    while let Some(start) = value
        .as_bytes()
        .windows(4)
        .position(|bytes| bytes.eq_ignore_ascii_case(b"url("))
    {
        value = &value[start + 4..];
        let end = value.find(')').ok_or("Invalid SVG resource URL.")?;
        let url = value[..end].trim().trim_matches(['\'', '"']);
        if !url.starts_with('#') {
            return Err("SVG external resource references are not supported.".into());
        }
        value = &value[end + 1..];
    }
    Ok(())
}

// resvg clips a group's layer to its canvas, but filter::apply_inner allocates
// using the transformed filter region itself. Check those actual tree regions,
// not just the root size. This is a work estimate, not a process memory limit.
fn check_render_budget(tree: &usvg::Tree, scale: f32) -> Result<(), String> {
    if tree.filters().len() > 16
        || tree
            .filters()
            .iter()
            .any(|filter| filter.primitives().len() > 16)
        || tree
            .filters()
            .iter()
            .map(|filter| filter.primitives().len())
            .sum::<usize>()
            > 64
    {
        return Err("SVG exceeds the normalized filter complexity limit.".into());
    }
    struct Budget {
        pixels: u64,
        nodes: usize,
        filters: usize,
    }
    fn region(rect: usvg::NonZeroRect, transform: tiny_skia::Transform) -> Result<u64, String> {
        let rect = rect
            .transform(transform)
            .ok_or("SVG has an invalid raster region.")?;
        if rect.x().abs() > 32_768.0
            || rect.y().abs() > 32_768.0
            || rect.width() > 4096.0
            || rect.height() > 4096.0
        {
            return Err("SVG exceeds the 4096-side raster region limit.".into());
        }
        Ok((rect.width().ceil() as u64 + 4) * (rect.height().ceil() as u64 + 4))
    }
    fn charge(budget: &mut Budget, pixels: u64) -> Result<(), String> {
        budget.pixels = budget
            .pixels
            .checked_sub(pixels)
            .ok_or("SVG exceeds the raster work limit.")?;
        Ok(())
    }
    fn visit(
        group: &usvg::Group,
        parent: tiny_skia::Transform,
        depth: usize,
        budget: &mut Budget,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("SVG exceeds the rendered nesting limit.".into());
        }
        let transform = parent.pre_concat(group.transform());
        if group.should_isolate() {
            let pixels = region(group.layer_bounding_box(), transform)?;
            // Account for an isolated layer plus its clip/mask intermediates.
            let copies = 1
                + 2 * u64::from(group.clip_path().is_some())
                + 3 * u64::from(group.mask().is_some());
            charge(budget, pixels.saturating_mul(copies))?;
        }
        for filter in group.filters() {
            budget.filters = budget
                .filters
                .checked_sub(1)
                .ok_or("SVG exceeds the filter application limit.")?;
            let pixels = region(filter.rect(), transform)?;
            for primitive in filter.primitives() {
                region(primitive.rect(), transform)?;
                use usvg::filter::Kind;
                let (sx, sy) = transform.get_scale();
                let (blur_x, blur_y, dx, dy) = match primitive.kind() {
                    Kind::GaussianBlur(blur) => {
                        (blur.std_dev_x().get(), blur.std_dev_y().get(), 0.0, 0.0)
                    }
                    Kind::DropShadow(shadow) => (
                        shadow.std_dev_x().get(),
                        shadow.std_dev_y().get(),
                        shadow.dx(),
                        shadow.dy(),
                    ),
                    Kind::Offset(offset) => (0.0, 0.0, offset.dx(), offset.dy()),
                    _ => (0.0, 0.0, 0.0, 0.0),
                };
                if blur_x * sx.abs() > 64.0
                    || blur_y * sy.abs() > 64.0
                    || (dx * sx).abs() > 4096.0
                    || (dy * sy).abs() > 4096.0
                {
                    return Err("SVG exceeds the filter blur or offset limit.".into());
                }
            }
            // resvg retains primitive results; allow additional input/scratch copies.
            charge(
                budget,
                pixels.saturating_mul(filter.primitives().len() as u64 + 6),
            )?;
        }
        for node in group.children() {
            budget.nodes = budget
                .nodes
                .checked_sub(1)
                .ok_or("SVG exceeds the rendered node limit.")?;
            if let usvg::Node::Group(group) = node {
                visit(group, transform, depth + 1, budget)?;
            }
            let mut result = Ok(());
            node.subroots(|root| {
                if result.is_ok() {
                    result = visit(root, transform, depth + 1, budget);
                }
            });
            result?;
        }
        Ok(())
    }
    visit(
        tree.root(),
        tiny_skia::Transform::from_scale(scale, scale),
        0,
        &mut Budget {
            pixels: MAX_RASTER_WORK_PIXELS,
            nodes: MAX_EXPANDED_NODES,
            filters: 64,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="40">{body}</svg>"#)
    }

    #[test]
    fn renders_bundled_text_and_internal_use_on_transparent_pixels() {
        for href in ["#tile", "#tile ", "  #tile   "] {
            let source = svg(&format!(
                r##"<defs><rect id="tile" width="8" height="8" fill="#123456"/></defs><use href="{href}" x="2" y="2"/><text x="16" y="28" font-family="sans-serif" font-size="22" fill="#ff0000">Hello</text>"##,
            ));
            let image = image::load_from_memory(&Svg::parse(&source).unwrap().render().unwrap())
                .unwrap()
                .to_rgba8();
            assert_eq!(image.get_pixel(4, 4).0, [0x12, 0x34, 0x56, 255]);
            assert_eq!(image.get_pixel(119, 39).0, [0, 0, 0, 0]);
            assert!(
                image
                    .pixels()
                    .filter(|pixel| pixel[0] > 200 && pixel[1] == 0 && pixel[3] > 100)
                    .count()
                    > 50,
                "text should render for {href:?}"
            );
        }
    }

    #[test]
    fn bounds_intrinsics_before_raster_and_downscales_with_aspect_ratio() {
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4096" height="1024"><rect width="4096" height="1024" fill="blue"/></svg>"#;
        let svg = Svg::parse(source).unwrap();
        assert_eq!(svg.intrinsic_pixels, 4096 * 1024);
        assert_eq!(svg.output_pixels(), 2048 * 512);
        let image = image::load_from_memory(&svg.render().unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(image.dimensions(), (2048, 512));
        assert_eq!(image.get_pixel(100, 100).0, [0, 0, 255, 255]);
        for (width, height) in [(16_385, 1), (5000, 5000)] {
            let source = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"/>"#
            );
            assert!(Svg::parse(&source).err().unwrap().contains("dimensions"));
        }
    }

    #[test]
    fn rejects_resources_instead_of_returning_partial_graphics() {
        for href in [
            "file:///tmp/image.png",
            "https://example.invalid/a.png",
            "data:image/png;base64,AAAA",
            "data:image/svg+xml,%3Csvg/%3E",
            "#tile",
        ] {
            let source = svg(&format!(
                r#"<rect width="20" height="20"/><image href="{href}" width="10" height="10"/>"#
            ));
            assert!(Svg::parse(&source).err().unwrap().contains("references"));
        }
        for body in [
            r#"<use href="other.svg#tile"/>"#,
            r#"<rect fill="url(file:///tmp/paint.svg#color)" width="10" height="10"/>"#,
            r#"<style>rect {fill: url('https://example.invalid/paint.svg#color')}</style><rect width="10" height="10"/>"#,
            "<script>alert(1)</script>",
            "<foreignObject/>",
            "<animate/>",
            "<feTurbulence/>",
        ] {
            assert!(Svg::parse(&svg(body)).is_err(), "{body}");
        }
    }

    #[test]
    fn rejects_invalid_oversize_dtd_and_expansion_complexity() {
        assert!(Svg::parse("<svg>").is_err());
        assert!(Svg::parse(&" ".repeat(MAX_SOURCE_BYTES as usize + 1)).is_err());
        assert!(
            Svg::parse(&format!(
                "<!DOCTYPE svg [<!ENTITY x 'text'>]>{}",
                svg("<text>&x;</text>")
            ))
            .is_err()
        );
        assert!(Svg::parse(&svg(&"<rect/>".repeat(MAX_NODES as usize))).is_err());
        assert!(
            Svg::parse(&svg(&format!(
                "{}{}",
                "<g>".repeat(MAX_DEPTH),
                "</g>".repeat(MAX_DEPTH)
            )))
            .is_err()
        );
        assert!(
            Svg::parse(&svg(
                r##"<defs><g id="a"><use href="#a"/></g></defs><use href="#a"/>"##
            ))
            .err()
            .unwrap()
            .contains("complexity")
        );
        let mut body = String::from(r#"<defs><g id="a0"><rect width="1" height="1"/></g>"#);
        for n in 1..15 {
            body.push_str(&format!(
                r##"<g id="a{n}"><use href="#a{}"/><use href="#a{}"/></g>"##,
                n - 1,
                n - 1
            ));
        }
        body.push_str(r##"</defs><use href="#a14"/>"##);
        assert!(
            Svg::parse(&svg(&body))
                .err()
                .unwrap()
                .contains("complexity")
        );
        let path = "M0 0 ".repeat(20_000);
        for duplicate in [false, true] {
            for (id, href) in [
                ("p", "#p"),
                ("p", "#p "),
                ("p", "  #p   "),
                ("p", "#p&#x20;"),
                ("p&#x9;", "#p&#x9;"),
            ] {
                let decoy = if duplicate {
                    format!(r#"<path id="{id}" d="M0 0"/>"#)
                } else {
                    String::new()
                };
                let source = svg(&format!(
                    r#"<defs><path id="{id}" d="{path}"/>{decoy}</defs>{}"#,
                    format!(r#"<use href="{href}"/>"#).repeat(50)
                ));
                let error = Svg::parse(&source).err().unwrap();
                let expected = if duplicate {
                    "IDs must be unique"
                } else {
                    "expanded source"
                };
                assert!(error.contains(expected), "{id:?}, {href:?}: {error}");
            }
        }
    }

    #[test]
    fn renders_static_drop_shadows_and_rejects_filter_work_over_budget() {
        let source = svg(
            r##"<defs><filter id="shadow" x="-50%" y="-50%" width="200%" height="200%"><feDropShadow dx="8" dy="8" stdDeviation="2" flood-color="black"/></filter></defs><rect x="10" y="10" width="20" height="20" fill="red" filter="url(#shadow)"/>"##,
        );
        let image = image::load_from_memory(&Svg::parse(&source).unwrap().render().unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(image.get_pixel(20, 20).0, [255, 0, 0, 255]);
        assert!(image.get_pixel(35, 35)[3] > 0);
        for body in [
            format!("<defs>{}</defs>", "<filter/>".repeat(17)),
            format!(r#"<defs><filter id="f">{}</filter></defs>"#, "<feOffset/>".repeat(17)),
            r##"<defs><filter id="f" filterUnits="userSpaceOnUse" width="100000" height="100000"><feGaussianBlur stdDeviation="2"/></filter></defs><rect width="20" height="20" filter="url(#f)"/>"##.into(),
            r##"<defs><filter id="f"><feGaussianBlur stdDeviation="100000"/></filter></defs><rect width="20" height="20" filter="url(#f)"/>"##.into(),
            format!(r##"<defs><filter id="f"><feOffset/></filter></defs>{}"##, r##"<rect width="2" height="2" filter="url(#f)"/>"##.repeat(65)),
        ] {
            assert!(Svg::parse(&svg(&body)).is_err(), "{body}");
        }
        let source = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="2048" height="2048"><defs><filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="2048" height="2048">{}</filter></defs><rect width="2048" height="2048" filter="url(#f)"/></svg>"##,
            "<feOffset/>".repeat(16)
        );
        assert!(Svg::parse(&source).err().unwrap().contains("raster work"));
    }
}
