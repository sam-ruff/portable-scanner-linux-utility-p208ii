use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use p208ii::encode::FileFormat;
use p208ii::output::default_output_dir;
use p208ii::params::{ColourMode, ScanSettings};
use p208ii::session::{Backend, Event, SessionOptions, open_scanner, run_worker};
use p208ii::simulator::PaperSupply;
use p208ii::{button, gui, install};

/// Scans receipts with a Canon imageFORMULA P-208II. Opens the app when run
/// without a command.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Use a pretend scanner instead of real hardware
    #[arg(long, global = true)]
    simulate: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Scan from the command line
    Scan(ScanArgs),
    /// Show scanner details and usage counters
    Info,
    /// Install the app and enable the scanner's blue button for this user
    Install,
    /// Remove the installed app and button listener, keeping scans
    Uninstall,
    /// Listen for the scanner's blue button (normally started at login)
    WatchButton,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Colour,
    Grey,
    Bw,
}

impl From<ModeArg> for ColourMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Colour => ColourMode::Colour,
            ModeArg::Grey => ColourMode::Grey,
            ModeArg::Bw => ColourMode::BlackWhite,
        }
    }
}

#[derive(Args)]
struct ScanArgs {
    /// Folder to save scans in [default: ~/Pictures/Scans]
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ModeArg::Colour)]
    mode: ModeArg,
    #[arg(long, default_value_t = 300)]
    dpi: u16,
    /// Scan both sides of each sheet
    #[arg(long)]
    duplex: bool,
    /// Width of the scan area, centred on the feeder [default: full width]
    #[arg(long)]
    width_mm: Option<u32>,
    /// Longest page to accept [default: scanner maximum]
    #[arg(long)]
    length_mm: Option<u32>,
    /// Keep waiting for more receipts until interrupted
    #[arg(long)]
    continuous: bool,
    #[arg(long, value_enum, default_value_t = FormatArg::Png)]
    format: FormatArg,
    /// Trim the scanner background from around each receipt
    #[arg(long)]
    smart_crop: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Png,
    Jpeg,
    Tiff,
    /// One PDF per sheet, with the back as a second page
    Pdf,
}

impl From<FormatArg> for FileFormat {
    fn from(format: FormatArg) -> Self {
        match format {
            FormatArg::Png => FileFormat::Png,
            FormatArg::Jpeg => FileFormat::Jpeg,
            FormatArg::Tiff => FileFormat::Tiff,
            FormatArg::Pdf => FileFormat::Pdf,
        }
    }
}

fn backend(simulate: bool, supply: PaperSupply) -> Backend {
    if !simulate {
        return Backend::Usb;
    }
    Backend::Simulated {
        supply,
        read_delay: Duration::from_millis(40),
    }
}

fn scan(args: ScanArgs, backend: Backend) -> ExitCode {
    let options = SessionOptions {
        settings: ScanSettings {
            mode: args.mode.into(),
            dpi: args.dpi,
            duplex: args.duplex,
            page_width_mm: args.width_mm,
            page_length_mm: args.length_mm,
            ..ScanSettings::default()
        },
        continuous: args.continuous,
        poll_interval: Duration::from_millis(500),
        format: args.format.into(),
        smart_crop: args.smart_crop,
    };
    let output = args.output.unwrap_or_else(default_output_dir);

    // Keep the sender so the worker only stops when the scan is done
    let (_command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let worker =
        std::thread::spawn(move || run_worker(backend, output, options, command_rx, event_tx));

    let mut code = ExitCode::SUCCESS;
    for event in event_rx {
        match event {
            Event::WaitingForPaper => eprintln!("Insert a receipt to scan"),
            Event::Saved { paths, .. } => {
                for path in paths {
                    println!("{}", path.display());
                }
            }
            Event::Finished { saved } => eprintln!("Saved {saved} scan(s)"),
            Event::Failed { message } => {
                eprintln!("Error: {message}");
                code = ExitCode::FAILURE;
            }
            Event::Connected { .. } | Event::Preparing | Event::Scanning => {}
        }
    }
    if worker.join().is_err() {
        return ExitCode::FAILURE;
    }
    code
}

fn info(backend: Backend) -> ExitCode {
    let scanner = match open_scanner(&backend) {
        Ok(scanner) => scanner,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let info = scanner.info();
    let caps = &info.capabilities;
    println!("Scanner:      {} {}", info.vendor, info.model);
    println!("Firmware:     {}", info.firmware);
    println!("Resolutions:  {:?} dpi", caps.resolutions);
    println!(
        "Largest page: {} x {} mm",
        caps.max_width * 254 / 12000,
        caps.max_length * 254 / 12000
    );
    match scanner.paper_loaded() {
        Ok(loaded) => println!("Paper loaded: {}", if loaded { "yes" } else { "no" }),
        Err(err) => println!("Paper loaded: unknown ({err})"),
    }
    match scanner.counters() {
        Ok(counters) => {
            println!("Pages fed:    {}", counters.total);
            println!("Since roller: {}", counters.since_roller_service);
        }
        Err(err) => println!("Counters:     unavailable ({err})"),
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(command) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
                .init();
            match command {
                CliCommand::Install => return command_result(install::install()),
                CliCommand::Uninstall => return command_result(install::uninstall()),
                CliCommand::WatchButton => return command_result(button::watch()),
                _ => {}
            }
            let _lock = if cli.simulate {
                None
            } else {
                match button::lock_application() {
                    Ok(lock) => Some(lock),
                    Err(err) => return command_result(Err(err)),
                }
            };
            match command {
                CliCommand::Scan(args) => scan(args, backend(cli.simulate, PaperSupply::Sheets(3))),
                CliCommand::Info => info(backend(cli.simulate, PaperSupply::Sheets(0))),
                _ => ExitCode::SUCCESS,
            }
        }
        None => {
            let _lock = match button::lock_application() {
                Ok(lock) => lock,
                Err(err) => return command_result(Err(err)),
            };
            let logs = gui::init_logging();
            let backend = backend(cli.simulate, PaperSupply::Every(Duration::from_secs(4)));
            match gui::run(backend, default_output_dir(), logs) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("Error: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn command_result(result: Result<(), p208ii::error::ScanError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}
