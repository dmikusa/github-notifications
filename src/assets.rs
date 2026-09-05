use rust_embed::RustEmbed;

/// Static web assets embedded into the binary at compile time.
#[derive(RustEmbed)]
#[folder = "ui/"]
pub struct Assets;

/// Order in which frontend JS files are concatenated into `/app.js`.
///
/// Keep this list in dependency order: later files may reference globals set
/// by earlier files. Add new modules here when they are introduced. Vendored
/// third-party files (e.g. htmx) come first and are not expected to carry a
/// module header marker.
pub const JS_BUNDLE: &[&str] = &[
    "vendor/htmx.js",
    "js/api.js",
    "js/state.js",
    "js/components/table.js",
    "js/components/filters.js",
    "js/views/queue.js",
    "js/views/inbox.js",
    "js/views/repos.js",
    "js/views/workspaces.js",
    "js/main.js",
];

/// Concatenate the frontend JS modules into a single script. Missing files are
/// skipped so the bundle works while modules are still being introduced.
pub fn app_js() -> String {
    let mut bundle = String::new();
    for path in JS_BUNDLE {
        if let Some(file) = Assets::get(path) {
            bundle.push_str(&String::from_utf8_lossy(&file.data));
            bundle.push('\n');
        }
    }
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_contains_each_module() {
        let bundle = app_js();
        assert!(!bundle.is_empty());
        for path in JS_BUNDLE {
            if Assets::get(path).is_some() {
                // Only our own modules carry the header marker; vendored files
                // (under vendor/) are expected to be raw third-party JS.
                if path.starts_with("vendor/") {
                    continue;
                }
                let marker = format!("{path}:");
                assert!(
                    bundle.contains(&marker),
                    "module {path} missing from bundle"
                );
            }
        }
    }

    #[test]
    fn index_html_embedded() {
        let file = Assets::get("index.html").expect("index.html embedded");
        assert!(!file.data.is_empty());
    }
}
