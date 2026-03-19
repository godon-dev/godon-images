mod auth;
mod component;

use clap::Parser;
use std::env;

#[derive(Parser, Debug)]
#[command(name = "godon-seeder")]
#[command(about = "Godon Seeder - Deploy Godon optimization components to Windmill", long_about = None)]
struct Args {
    #[arg(help = "Source directories to scan for components")]
    directories: Vec<String>,

    #[arg(short, long, help = "Enable verbose logging")]
    verbose: bool,

    #[arg(long, help = "Maximum connection retry attempts", default_value = "30")]
    max_retries: u32,

    #[arg(long, help = "Delay between retries in seconds", default_value = "2")]
    retry_delay: u64,
}

fn main() {
    let args = Args::parse();

    let log_level = if args.verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .filter_module("godon_seeder", log_level)
        .init();

    if let Err(e) = auth::setup_windmill_env(args.max_retries, args.retry_delay) {
        log::error!("Failed to setup Windmill authentication: {}", e);
        std::process::exit(1);
    }

    let source_dirs = if args.directories.is_empty() {
        let godon_dir = env::var("GODON_DIR").unwrap_or_else(|_| "/godon".to_string());
        log::info!("No source directories specified, using GODON_DIR: {}", godon_dir);
        vec![godon_dir]
    } else {
        args.directories
    };

    let windmill_workspace = env::var("WINDMILL_WORKSPACE").unwrap_or_else(|_| "godon".to_string());

    log::info!("Starting Godon Seeder");
    log::info!("Source directories: {}", source_dirs.join(", "));
    log::info!(
        "Windmill URL: {}",
        env::var("WINDMILL_BASE_URL").unwrap_or_else(|_| "http://localhost:8000".to_string())
    );
    log::info!("Workspace: {}", windmill_workspace);

    if args.verbose {
        log::debug!("Configuration:");
        log::debug!("  Base URL: {}", env::var("WINDMILL_BASE_URL").unwrap_or_default());
        log::debug!("  Workspace: {}", windmill_workspace);
        log::debug!("  Email: {}", env::var("WINDMILL_EMAIL").unwrap_or_default());
        log::debug!("  Max Retries: {}", args.max_retries);
        log::debug!("  Retry Delay: {}", args.retry_delay);
    }

    match component::seed_workspace(&source_dirs, &windmill_workspace, args.max_retries, args.retry_delay) {
        Ok(failures) => {
            if failures > 0 {
                std::process::exit(1);
            }
        }
        Err(e) => {
            log::error!("Seeding failed: {}", e);
            std::process::exit(1);
        }
    }
}
