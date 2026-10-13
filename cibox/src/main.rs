use cibox::cli::{Cli, Commands};
use cibox::error::Result;
use clap::error::ErrorKind;
use clap::{ArgMatches, CommandFactory, FromArgMatches};

/// Parse the command line, building the emblem only once clap has said it is
/// about to print help.
///
/// The emblem rides above the help text, as a real image on terminals with a
/// graphics protocol and in block letters elsewhere, and clap is handed it as
/// `before_help` so it appears nowhere else. Building it is far from free
/// though: on a terminal that speaks a graphics protocol the SVG is
/// rasterized and rescaled, which takes longer than any of the commands spend
/// on the work they were asked to do. Handing it to clap up front made every
/// invocation pay for a picture only `--help` ever shows, so it is built here
/// instead, on the one path that displays it.
fn matches() -> ArgMatches {
    match Cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) =>
        {
            let mut command = Cli::command();
            if let Some(emblem) = cilki::terminal::before_help(&cilki::emblem::CIBOX, 56, 7) {
                command = command.before_help(emblem);
            }
            // The same arguments ask for the same help, now rendered from a
            // template that carries the emblem
            match command.try_get_matches() {
                Ok(matches) => matches,
                Err(err) => err.exit(),
            }
        }
        Err(err) => err.exit(),
    }
}

fn main() -> Result<()> {
    let cli = Cli::from_arg_matches(&matches())?;

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
