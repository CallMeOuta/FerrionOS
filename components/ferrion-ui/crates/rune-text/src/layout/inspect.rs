use parley::{Layout, PositionedLayoutItem};
use skrifa::MetadataProvider;

use crate::Ink;

#[derive(Clone, Debug)]
pub struct InspectedGlyph {
    pub gid: u32,
    pub isolated_gid: Option<u32>,
    pub text: String,
    pub cluster: [usize; 2],
    pub x: f32,
    pub advance: f32,
}

#[derive(Clone, Debug)]
pub struct InspectedRun {
    pub rtl: bool,
    pub family: String,
    pub font_index: u32,
    pub offset: f32,
    pub end_x: f32,
    pub notdef_count: usize,
    pub glyphs: Vec<InspectedGlyph>,
}

fn family_name(font: &skrifa::FontRef<'_>) -> String {
    let typographic = font
        .localized_strings(skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME)
        .english_or_first()
        .map(|s| s.chars().collect::<String>());
    if let Some(name) = typographic.filter(|s| !s.is_empty()) {
        return name;
    }
    font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
        .english_or_first()
        .map(|s| s.chars().collect::<String>())
        .unwrap_or_default()
}

pub fn inspect_layout(layout: &Layout<Ink>, text: &str) -> Vec<InspectedRun> {
    let mut runs = Vec::new();
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
            let run = glyph_run.run();
            let font_data = run.font();
            let font_ref = skrifa::FontRef::from_index(font_data.data.as_ref(), font_data.index).ok();
            let family = font_ref.as_ref().map(family_name).unwrap_or_default();
            let charmap = font_ref.as_ref().map(MetadataProvider::charmap);
            let mut glyphs = Vec::new();
            let mut notdef_count = 0usize;
            let mut x = glyph_run.offset();
            for cluster in run.visual_clusters() {
                let range = cluster.text_range();
                let slice = text.get(range.clone()).unwrap_or("");
                let isolated_gid = slice
                    .chars()
                    .next()
                    .and_then(|ch| charmap.as_ref().and_then(|cm| cm.map(ch)))
                    .map(|g| g.to_u32());
                for glyph in cluster.glyphs() {
                    if glyph.id == 0 {
                        notdef_count += 1;
                    }
                    glyphs.push(InspectedGlyph {
                        gid: glyph.id,
                        isolated_gid,
                        text: slice.to_owned(),
                        cluster: [range.start, range.end],
                        x: x + glyph.x,
                        advance: glyph.advance,
                    });
                    x += glyph.advance;
                }
            }
            runs.push(InspectedRun {
                rtl: run.is_rtl(),
                family,
                font_index: font_data.index,
                offset: glyph_run.offset(),
                end_x: x,
                notdef_count,
                glyphs,
            });
        }
    }
    runs
}
