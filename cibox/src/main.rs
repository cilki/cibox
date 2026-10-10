use cibox::cli::{Cli, Commands};
use cibox::error::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Update {
            config,
            platform,
            force,
            rules,
        }) => cibox::cli::commands::handle_update(&config, platform, force, &rules),
        Some(Commands::Validate { config }) => cibox::cli::commands::handle_validate(&config),
        Some(Commands::Editor { dir }) => cibox::editor::run_with_args(&dir, None),
        Some(Commands::Detect { dir }) => cibox::cli::commands::handle_detect(&dir),
        Some(Commands::Lsp) => {
            let analyzer: roniker::RustAnalyzer = serde_json::from_str(include_str!(concat!(
                env!("OUT_DIR"),
                "/rust_analyzer.json"
            )))
            .expect("embedded rust_analyzer.json is valid");
            tokio::runtime::Runtime::new()?.block_on(roniker::serve(analyzer, true));
            Ok(())
        }
        _ => {
            // Default to editor if no command specified
            cibox::editor::run_with_args(".", None)
        }
    }
}
