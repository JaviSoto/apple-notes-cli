use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

fn enabled() -> bool {
    if std::env::var_os("APPLE_NOTES_FORCE_PROGRESS").is_some() {
        return true;
    }
    if std::env::var_os("NO_PROGRESS").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}

pub fn spinner(msg: &str) -> Option<ProgressBar> {
    if !enabled() {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(msg.to_string());
    Some(pb)
}

pub fn bar(len: u64, msg: &str) -> Option<ProgressBar> {
    if !enabled() {
        return None;
    }
    let pb = ProgressBar::new(len);
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg} {wide_bar} {pos}/{len}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(msg.to_string());
    Some(pb)
}
