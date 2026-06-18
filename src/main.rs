mod app;
mod audio;
mod battery;
mod config;
mod cpu;
mod display;
mod gpu;
mod sysutil;
mod system;

fn main() {
    if let Err(e) = app::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
