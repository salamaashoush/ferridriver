//! One module per command, and the dispatch that picks between them.
//!
//! `main` parses and resolves; everything a command actually does lives here,
//! so adding a command means adding a file and one arm rather than growing a
//! single function past the point anyone can read it.

pub mod bdd;
pub mod bootstrap;
pub mod codegen;
pub mod completions;
pub mod config;
pub mod ext;
pub mod init;
pub mod install;
pub mod instance;
pub mod mcp;
pub mod merge;
pub mod run;
pub mod rust_test;
pub mod script_setup;
pub mod session;
pub mod suite;
pub mod test;
pub mod trace;

use ferridriver_config::FerridriverConfig;

use crate::cli;

/// Run the command the arguments named.
///
/// Every arm is boxed rather than awaited inline: this frame is the union of
/// all of them, so inline futures would put every command's stack behind
/// whichever single one is running.
pub async fn dispatch(
  args: cli::Cli,
  config: FerridriverConfig,
  startup: &ferridriver_config::Startup,
  contributed: bootstrap::ContributedDefaults,
) -> anyhow::Result<()> {
  // Global flags this command still needs after the parse: `session` spawns a
  // detached host and has to hand it the same config layers this process
  // resolved, which `--config` / `--no-inherit` describe.
  let config_path = args.config.clone();
  let inherit = !args.no_inherit;

  match args.command {
    cli::Command::Init(init_args) => init::run(&init_args),
    cli::Command::Mcp(mcp_args) => Box::pin(mcp::run(config, mcp_args)).await,
    cli::Command::Bdd(bdd_args) => Box::pin(bdd::run(config, bdd_args)).await,
    cli::Command::Test(test_args) => Box::pin(test::run(config, test_args)).await,
    cli::Command::RustTest(test_args) => {
      if test_args.ui {
        Box::pin(rust_test::ui::run(config, test_args)).await
      } else if test_args.watch {
        Box::pin(rust_test::run_watch(config, test_args)).await
      } else {
        rust_test::run(&test_args)
      }
    },
    cli::Command::Run(run_args) => Box::pin(run::run(config, run_args)).await,
    cli::Command::Install(install_args) => Box::pin(install::run(install_args)).await,
    cli::Command::Codegen(codegen_args) => Box::pin(codegen::run(codegen_args)).await,
    cli::Command::Session(session_args) => {
      let origin = session::ConfigOrigin {
        explicit: config_path.as_deref(),
        inherit,
      };
      Box::pin(session::run(config, origin, session_args)).await
    },
    cli::Command::Config(config_args) => config::run_config(startup, contributed, &config_args),
    cli::Command::Doctor(doctor_args) => Box::pin(config::doctor::run(startup, contributed, doctor_args)).await,
    cli::Command::Ext(ext_args) => Box::pin(ext::run(config, ext_args)).await,
    cli::Command::Trace(trace_args) => Box::pin(trace::run(&config, trace_args)).await,
    cli::Command::MergeReports(merge_args) => Box::pin(merge::run(config, merge_args)).await,
    cli::Command::Completions(completions_args) => completions::run(&completions_args),
  }
}
