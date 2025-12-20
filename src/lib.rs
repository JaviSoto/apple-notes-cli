mod backup;
mod cli;
mod db;
mod fixture;
mod model;
mod progress;
mod render;
mod tables;
mod transport;

use anyhow::Context;
use clap::Parser;

pub fn run() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    let backend = transport::make_backend(args.fixture.clone(), args.backend)?;

    cli::dispatch(args, backend).context("command failed")
}
