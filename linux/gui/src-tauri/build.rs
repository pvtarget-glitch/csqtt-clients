fn main() {
    // Прокси в WebView на macOS — функции фреймворка Network из macOS 14.
    // Слабая ссылка: на старых версиях приложение запускается, а прокси
    // просто не используется (см. webview_proxy::supported).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,Network");
    }
    tauri_build::build()
}
