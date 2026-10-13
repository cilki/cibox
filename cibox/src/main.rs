use cibox::cli::{Cli, Commands};
use cibox::error::Result;
use clap::{CommandFactory, FromArgMatches};

fn main() -> Result<()> {
    // The emblem rides above the help text, as a real image on terminals with
    // a graphics protocol and in block letters elsewhere. clap is handed it as
    // `before_help` so it appears only when help is actually printed, and
    // never when the output is a pipe or a file.
    let mut command = Cli::command();
    if let Some(emblem) = cilki::terminal::before_help(&cilki::emblem::CIBOX, 56, 7) {
        command = command.before_help(emblem);
    }
    let cli = Cli::from_arg_matches(&command.get_matches())?;

    match cli.command {
        Some(Commands::Update {
            config,
            platform,
            force,
            rules,
        }) => cibox::cli::commands::handle_update(&config, &platform.selected(), force, &rules),
        Some(Commands::Validate { config }) => cibox::cli::commands::handle_validate(&config),
        Some(Commands::Editor { dir, platform }) => {
            cibox::editor::run_with_args(&dir, platform.first())
        }
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
