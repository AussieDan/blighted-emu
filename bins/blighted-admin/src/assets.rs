// SPDX-License-Identifier: AGPL-3.0-or-later
//! The web files, built into the program (no build step, no package).

/// Content type of HTML pages.
pub const HTML: &str = "text/html; charset=utf-8";
/// Content type of style sheets.
pub const CSS: &str = "text/css; charset=utf-8";
/// Content type of script modules.
pub const JS: &str = "text/javascript; charset=utf-8";

/// One web file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// `Content-Type` value.
    pub content_type: &'static str,
    /// The file.
    pub body: &'static str,
}

/// The web file of `name`; `None` for any other name.
pub fn asset(name: &str) -> Option<Asset> {
    let (content_type, body) = match name {
        "index.html" => (HTML, include_str!("../web/index.html")),
        "style.css" => (CSS, include_str!("../web/style.css")),
        "map.html" => (HTML, include_str!("../web/map.html")),
        "map.js" => (JS, include_str!("../web/map.js")),
        "live.html" => (HTML, include_str!("../web/live.html")),
        "live.js" => (JS, include_str!("../web/live.js")),
        "tiles.js" => (JS, include_str!("../web/tiles.js")),
        "status.js" => (JS, include_str!("../web/status.js")),
        "entity.js" => (JS, include_str!("../web/entity.js")),
        "palette.js" => (JS, include_str!("../web/palette.js")),
        "theme.js" => (JS, include_str!("../web/theme.js")),
        "view.js" => (JS, include_str!("../web/view.js")),
        "hex.js" => (JS, include_str!("../web/hex.js")),
        "tables.js" => (JS, include_str!("../web/tables.js")),
        "slots.js" => (JS, include_str!("../web/slots.js")),
        "login.html" => (HTML, include_str!("../web/login.html")),
        "login.js" => (JS, include_str!("../web/login.js")),
        "gm.js" => (JS, include_str!("../web/gm.js")),
        "gm.html" => (HTML, include_str!("../web/gm.html")),
        "gm-page.js" => (JS, include_str!("../web/gm-page.js")),
        "gm-player.js" => (JS, include_str!("../web/gm-player.js")),
        "studio.html" => (HTML, include_str!("../web/studio.html")),
        "studio.js" => (JS, include_str!("../web/studio.js")),
        "apply.js" => (JS, include_str!("../web/apply.js")),
        "design.js" => (JS, include_str!("../web/design.js")),
        "editors.js" => (JS, include_str!("../web/editors.js")),
        "records.html" => (HTML, include_str!("../web/records.html")),
        "records.js" => (JS, include_str!("../web/records.js")),
        "record-edit.js" => (JS, include_str!("../web/record-edit.js")),
        "todo.html" => (HTML, include_str!("../web/todo.html")),
        "scripts.html" => (HTML, include_str!("../web/scripts.html")),
        "scripts.js" => (JS, include_str!("../web/scripts.js")),
        "script-edit.js" => (JS, include_str!("../web/script-edit.js")),
        "todo.js" => (JS, include_str!("../web/todo.js")),
        "todo-model.js" => (JS, include_str!("../web/todo-model.js")),
        "captures.html" => (HTML, include_str!("../web/captures.html")),
        "captures.js" => (JS, include_str!("../web/captures.js")),
        "live3d.html" => (HTML, include_str!("../web/live3d.html")),
        "live3d.js" => (JS, include_str!("../web/live3d.js")),
        "three.module.min.js" => (JS, include_str!("../web/vendor/three/three.module.min.js")),
        "OrbitControls.js" => (JS, include_str!("../web/vendor/three/OrbitControls.js")),
        _ => return None,
    };
    Some(Asset { content_type, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_files_have_their_type_and_others_none() {
        assert_eq!(
            asset("index.html").map(|file| file.content_type),
            Some(HTML)
        );
        assert_eq!(asset("style.css").map(|file| file.content_type), Some(CSS));
        for script in [
            "live.js",
            "tiles.js",
            "status.js",
            "entity.js",
            "palette.js",
            "theme.js",
            "map.js",
            "view.js",
            "hex.js",
            "tables.js",
            "slots.js",
            "captures.js",
        ] {
            assert_eq!(asset(script).map(|file| file.content_type), Some(JS));
        }
        assert_eq!(asset("map.html").map(|file| file.content_type), Some(HTML));
        assert_eq!(asset("live.html").map(|file| file.content_type), Some(HTML));
        assert_eq!(
            asset("captures.html").map(|file| file.content_type),
            Some(HTML)
        );
        assert!(asset("view.test.mjs").is_none());
        assert!(asset("../Cargo.toml").is_none());
        assert!(asset("missing.js").is_none());
    }
}
