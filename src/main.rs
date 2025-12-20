fn main() {
    #[cfg(unix)]
    unsafe {
        // Avoid panics when piping output (e.g. `apple-notes ... | head`).
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    if let Err(err) = apple_notes_cli::run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
