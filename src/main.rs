//! Workbench for LiveKit: publish a camera or synthetic video into a LiveKit
//! room, receive it back, and see what every stage of the pipeline does.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod capabilities;
mod capture;
mod headless;
mod health;
mod local_server;
mod profiles;
mod session;
mod stats;
mod throttle;
mod ui;
mod version;
mod viewer;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("{}", version::summary());
            return Ok(());
        }
        Some("--caps") => {
            capabilities::print_probe();
            return Ok(());
        }
        Some("--headless") => {
            std::process::exit(headless::run(&args[1..]));
        }
        Some("--help" | "-h") => {
            println!("{} {}\n\nUsage:\n  livekit-workbench              open the app\n  livekit-workbench --version    print version and SDK information\n  livekit-workbench --headless   scripted publish/receive test (see --headless --help)", version::APP_NAME, version::APP_VERSION);
            return Ok(());
        }
        _ => {}
    }
    capture::init_camera_access();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([980.0, 640.0])
            .with_title(version::APP_NAME)
            .with_fullscreen(std::env::var_os("WB_FULLSCREEN").is_some()),
        ..Default::default()
    };
    eframe::run_native(
        version::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
