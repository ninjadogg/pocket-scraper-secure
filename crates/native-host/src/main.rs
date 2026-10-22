use pocket_scraper_host::{handler, install};
use std::process::ExitCode;

const USAGE: &str = "pocket-scraper-host: Pocket Scraper Secure's native messaging host.

Chrome starts it for the extension. To set it up:
  pocket-scraper-host install [--browser chrome|chromium|brave|edge]... [--extension-id ID]
  pocket-scraper-host install-browser      (also done by install)
  pocket-scraper-host collector configure --endpoint URL --bucket NAME [--region R] [--prefix P] --access-key-id ID
                                           (read-only key; secret from PSS_S3_SECRET_ACCESS_KEY or stdin)
  pocket-scraper-host uninstall [--purge-profiles]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => install::install(&args[1..]),
        Some("uninstall") => install::uninstall(&args[1..]),
        Some("collector") if args.get(1).map(String::as_str) == Some("configure") => pocket_scraper_host::transport::configure(&args[2..], &pocket_scraper_host::transport::collector_config_path()),
        Some("install-browser") => pocket_scraper_host::headless::install(&mut |m| eprintln!("{m}")).map(|p| eprintln!("Browser ready: {}", p.display())),
        Some("--version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // Chrome passes the calling extension's origin first.
        Some(origin) if origin.starts_with("chrome-extension://") => {
            let allowed = format!("chrome-extension://{}/", install::allowed_extension_id());
            if origin != allowed {
                eprintln!("Refusing connection from {origin}");
                return ExitCode::from(1);
            }
            pocket_scraper_host::browser::keep_stdio_to_ourselves();
            handler::serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).map_err(|e| e.to_string())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
