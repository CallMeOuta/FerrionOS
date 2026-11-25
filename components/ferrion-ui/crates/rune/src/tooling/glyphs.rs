use serde_json::{Value, json};

use crate::el::Flow;
use crate::runtime::RuntimeCore;
use crate::text::{fontconfig_files_loaded, fontconfig_system_families, inspect_layout};

use super::trace::r2;

pub(crate) fn glyphs_json_core(rt: &RuntimeCore, real_t: f64) -> Value {
    let mut elements = Vec::new();
    for node in &rt.tree.nodes {
        let (Some(spec), Some(text_layout)) = (&node.text, &node.text_layout) else {
            continue;
        };
        let Some(layout) = text_layout.single() else {
            continue;
        };
        let runs: Vec<Value> = inspect_layout(layout, &spec.content)
            .into_iter()
            .map(|run| {
                let glyphs: Vec<Value> = run
                    .glyphs
                    .into_iter()
                    .map(|g| {
                        json!({
                            "gid": g.gid,
                            "isolatedGid": g.isolated_gid,
                            "text": g.text,
                            "cluster": g.cluster,
                            "x": r2(g.x),
                            "advance": r2(g.advance),
                        })
                    })
                    .collect();
                json!({
                    "rtl": run.rtl,
                    "family": run.family,
                    "fontIndex": run.font_index,
                    "offset": r2(run.offset),
                    "endX": r2(run.end_x),
                    "notdefCount": run.notdef_count,
                    "glyphs": glyphs,
                })
            })
            .collect();
        let mut entry = json!({
            "path": node.path,
            "text": spec.content,
            "flow": match node.flow {
                Flow::Ltr => "ltr",
                Flow::Rtl => "rtl",
            },
            "layoutRtl": layout.is_rtl(),
            "runs": runs,
        });
        if let Some(key) = &node.key {
            entry["key"] = key.clone().into();
        }
        elements.push(entry);
    }
    let dir = rt.text.dir_font_summary();
    json!({
        "t": real_t,
        "scale": rt.env.scale,
        "fontStats": {
            "dirFiles": dir.files,
            "dirFamilies": dir.families,
            "fontconfigFiles": fontconfig_files_loaded(),
            "fontconfigFamilies": fontconfig_system_families(),
        },
        "elements": elements,
    })
}
