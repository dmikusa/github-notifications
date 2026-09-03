/// Best-effort open of a URL in the platform default browser.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = "xdg-open";
    #[cfg(target_os = "windows")]
    let command = "cmd";

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new(command)
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let result = std::process::Command::new(command).arg(url).spawn();

    if let Err(e) = result {
        tracing::warn!("could not open browser: {e}");
    }
}
