mod app;
mod config;
mod error;
mod gpio;
mod protocol;
mod scheduler;
mod session;
mod transport;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), error::AppError> {
    match app::parse_args(std::env::args_os())? {
        app::ParseArgs::Help => {
            print!("{}", app::HELP);
            Ok(())
        }
        app::ParseArgs::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        app::ParseArgs::Run(cli) => {
            let service = config::ServiceConfig::load(cli.config.as_deref())?;
            let mock = app::MockMode::from_cli(cli.mock)?;

            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| error::AppError::Bootstrap(err.to_string()))?
                .block_on(app::run(app::AppConfig { service, mock }))
        }
    }
}
