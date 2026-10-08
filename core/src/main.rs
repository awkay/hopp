use clap::Parser;
use hopp_core::RenderEventLoop;
use sentry_utils::init_sentry;

/// Hopp Core - Remote Desktop Control System
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Sentry DSN
    #[arg(short, long)]
    sentry_dsn: Option<String>,

    /// Socket name
    #[arg(long)]
    socket_path: Option<String>,
}

fn main() -> Result<(), impl std::error::Error> {
    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let _guard = init_sentry("Core crashed".to_string(), args.sentry_dsn);

    #[cfg(target_os = "linux")]
    {
        /* This is needed for getting the system picker for screen sharing. */
        use glib::MainLoop;
        let main_loop = MainLoop::new(None, false);
        let _handle = std::thread::spawn(move || {
            main_loop.run();
        });
    }

    let socket_path = match args.socket_path {
        Some(path) => path,
        None => std::env::temp_dir()
            .join("core-socket")
            .to_string_lossy()
            .to_string(),
    };

    // Cursor badges need the system fonts on the main thread when a participant joins.
    let _ = std::thread::Builder::new()
        .name("font-warmup".to_string())
        .spawn(|| {
            hopp_core::utils::svg_renderer::system_fonts();
        });

    let render_event_loop = RenderEventLoop::new();
    render_event_loop.run(socket_path)
}
