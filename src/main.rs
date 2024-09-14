use anyhow::{anyhow, Result};
use clap::{Args, Parser, Subcommand};
use dirs::config_dir;
use discord_rich_presence::{
    activity::{Activity, Assets, Timestamps},
    DiscordIpc, DiscordIpcClient,
};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use simplelog::*;
use std::{
    ffi::OsStr,
    fs,
    io::ErrorKind,
    path::PathBuf,
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

pub fn log_init(log_level: LevelFilter) -> () {
    TermLogger::init(
        log_level,
        ConfigBuilder::new()
            .set_level_color(Level::Trace, Some(Color::Cyan))
            .set_level_color(Level::Debug, Some(Color::Blue))
            .set_level_color(Level::Info, Some(Color::Green))
            // .add_filter_allow_str("custom-app-presence")
            .build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .expect("logger should not be set more than once");
}

fn config_path() -> PathBuf {
    match config_dir() {
        Some(dir) => dir.join("carp"),
        None => PathBuf::from("./carp"),
    }
}

fn config_file() -> PathBuf {
    config_path().join("targets.json")
}

pub fn write_config(config: &Config) -> Result<()> {
    let ser_config = serde_json::to_string(&config)?;

    if !config_path().exists() {
        fs::create_dir_all(config_path())?;
    }

    Ok(fs::write(config_file(), ser_config)?)
}

pub fn get_config() -> Result<Config> {
    let config_file = match fs::read(&config_file()) {
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                warn!("Failed to read config file: {}", err);
                info!("Creating a blank config file");
                return Ok(Config::default());
            }
            return Err(anyhow!("Failed to read config file: {}", err));
        }
        Ok(config_file) => config_file,
    };

    match serde_json::from_str(&String::from_utf8(config_file)?) {
        Err(err) => Err(anyhow!("Failed to parse config file: {}", err)),
        Ok(config) => Ok(config),
    }
}

fn list_config(config: &Config, mut compact: bool, detailed: bool) {
    if config.targets.len() > 5 && !detailed {
        compact = true;
    }

    println!("Client ID: {}", config.client_id);

    for (index, target) in config.targets.iter().enumerate() {
        if compact {
            println!("{} - Process: {}", index, target.process_name);
            continue;
        }
        println!(
            "{} - Process: {}\n\tDisplay Text: {}\n\tImage URL/key: {}",
            index, target.process_name, target.display_name, target.image
        );
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub client_id: u64,
    pub targets: Vec<Target>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Target {
    pub process_name: String,
    pub display_name: String,
    pub image: String,
}

/// Returns the index of a process in the targets list or returns an error message if no process is found.
fn get_process_index(targets: &Vec<Target>, process: String) -> Result<usize> {
    match targets
        .iter()
        .position(|target| target.process_name == process)
    {
        None => Err(anyhow!("That process does not exist in the target list")),
        Some(index) => Ok(index),
    }
}

fn add_process(config: &mut Config, new_target: CliConfigAdd) -> Result<()> {
    if config
        .targets
        .iter()
        .any(|target| target.process_name == new_target.process)
    {
        return Err(anyhow!("That process already exists in the target list"));
    }

    let target = Target {
        process_name: new_target.process,
        display_name: new_target.display,
        image: new_target.image,
    };

    if let Some(index) = new_target.index {
        config.targets.insert(
            index.clamp(0, config.targets.len() as u32 - 1) as usize,
            target,
        )
    } else {
        config.targets.push(target);
    }

    Ok(())
}

fn remove_process(config: &mut Config, process: String) -> Result<()> {
    config
        .targets
        .remove(get_process_index(&config.targets, process)?);
    Ok(())
}

fn move_process(
    config: &mut Config,
    process: String,
    operation: ConfigReorderOperation,
) -> Result<()> {
    let index = get_process_index(&config.targets, process)?;
    let new_index = match operation {
        ConfigReorderOperation::Decrease => (index + 1).clamp(0, config.targets.len() - 1),
        ConfigReorderOperation::Increase => {
            (index as i32 - 1).clamp(0, config.targets.len() as i32 - 1) as usize
        }
        ConfigReorderOperation::Set(target_index) => {
            (target_index as usize).clamp(0, config.targets.len() - 1)
        }
    };

    if new_index == index {
        info!("Process priority did not change");
        return Ok(());
    }

    let target = config.targets.remove(index);
    config.targets.insert(new_index, target);

    Ok(())
}

fn edit_process(config: &mut Config, process: String, edits: CliConfigEdit) -> Result<()> {
    let index = get_process_index(&config.targets, process)?;

    if let Some(process) = edits.process_edit {
        if config
            .targets
            .iter()
            .any(|target| target.process_name == process)
        {
            return Err(anyhow!("That process already exists in the target list"));
        }

        config.targets[index].process_name = process;
    }

    if let Some(display) = edits.display {
        config.targets[index].display_name = display;
    }

    if let Some(image) = edits.image {
        config.targets[index].image = image;
    }

    Ok(())
}

fn main() -> Result<()> {
    log_init(LevelFilter::Debug);

    if std::env::args_os().len() <= 1 {
        app_loop(get_config()?);
    }
    let cli = Cli::parse();
    let mut config = get_config()?;

    match cli.subcommands {
        CliSubcommands::Run => app_loop(config),
        CliSubcommands::Config { subcommands } => match subcommands {
            CliConfig::Add(new_target) => add_process(&mut config, new_target),
            CliConfig::Edit { process, flags } => edit_process(&mut config, process, flags),
            CliConfig::Id { client_id } => Ok(config.client_id = client_id),
            CliConfig::List {
                force_compact,
                force_detailed,
            } => Ok(list_config(&config, force_compact, force_detailed)),
            CliConfig::Remove { process } => remove_process(&mut config, process),
            CliConfig::Reorder { process, flags } => {
                move_process(&mut config, process, flags.into())
            }
        }?,
    }

    write_config(&config)
}

fn app_loop(config: Config) -> ! {
    let mut processes =
        System::new_with_specifics(RefreshKind::new().with_processes(ProcessRefreshKind::new()));

    println!("Target processes:");
    for target in &config.targets {
        println!("{}", target.process_name);
    }

    let mut client = DiscordIpcClient::new(&config.client_id.to_string()).unwrap();

    debug!("Attempting to connect to Discord...");
    client.connect().unwrap();
    debug!("Connected");

    let mut last_detected_process = "";
    loop {
        processes.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
        for (index, target) in config.targets.iter().enumerate() {
            if let None = processes
                .processes_by_exact_name(OsStr::new(&target.process_name))
                .next()
            {
                if index == config.targets.len() - 1 && last_detected_process != "None" {
                    client.clear_activity().unwrap();
                    last_detected_process = "None";
                    info!("No process detected");
                }
                continue;
            }

            if last_detected_process == target.process_name {
                break;
            }

            last_detected_process = &target.process_name;
            info!("New process detected: {}", target.process_name);

            let mut details = String::new();
            let mut state = String::new();

            // This disgusting block of if statements just splits display names above 35 characters into
            // 2 lines (details & state) to hopefully mitigate any ellipses on the first line
            if target.display_name.chars().count() > 35 {
                let words: Vec<&str> = target.display_name.split_whitespace().collect();

                // blocks of text or exceedingly long words can just be forcefully split
                if words.len() <= 1 || words[0].chars().count() > 35 {
                    details = target.display_name.clone();
                    state = details.split_off(35);
                } else {
                    for (index, word) in words.iter().enumerate() {
                        // basically add words until it goes over 36 characters (36 instead of 35 to compensate for the
                        // dummy space at the beginning of the string) and then just get the remaining words and jam them
                        // into state
                        if details.chars().count() + word.chars().count() <= 35 {
                            details.push(' ');
                            details.push_str(word);
                            continue;
                        }
                        let (_, state_words) = words.split_at(index);
                        state.push_str(&state_words.join(" "));
                        break;
                    }
                }
            } else {
                details = target.display_name.clone();
            }

            let start_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs() as i64;

            let mut activity = Activity::new()
                .assets(Assets::new().large_image(&target.image))
                .details(details.trim())
                .timestamps(Timestamps::new().start(start_time));

            if !state.is_empty() {
                activity = activity.state(state.trim());
            }

            client.set_activity(activity).unwrap();

            break;
        }

        // Prevents Discord from forcefully closing the connection
        sleep(Duration::from_secs(1));
    }
}

#[derive(Debug, Parser)]
#[command(about = format!(
	"Custom App Rich Presence v{}\nBy Timbits\n\nA simple program to detect and display a Discord rich presence for any program",
	env!("CARGO_PKG_VERSION")
))]
struct Cli {
    #[command(subcommand)]
    subcommands: CliSubcommands,
}

#[derive(Debug, Subcommand)]
enum CliSubcommands {
    #[command(about = "Run the program")]
    Run,
    #[command(about = "Configure the config")]
    Config {
        #[command(subcommand)]
        subcommands: CliConfig,
    },
}

#[derive(Debug, Subcommand)]
enum CliConfig {
    #[command(about = "Add a process to the target list")]
    Add(CliConfigAdd),
    #[command(about = "Edit a process entry in the target list")]
    Edit {
        #[arg(help = "The process name (not necessarily the executable name)")]
        process: String,
        #[command(flatten)]
        flags: CliConfigEdit,
    },
    #[command(about = "Set the Discord client ID")]
    Id {
        #[arg(help = "The ID of your Discord client")]
        client_id: u64,
    },
    #[command(about = "List the config")]
    #[group(multiple = false)]
    List {
        #[arg(short = 'c', long, help = "Force the config output to be compact")]
        force_compact: bool,
        #[arg(short = 'd', long, help = "Force the config output to be detailed")]
        force_detailed: bool,
    },
    #[command(about = "Change the order of a process")]
    Reorder {
        #[arg(help = "The process name (not necessarily the executable name)")]
        process: String,
        #[command(flatten)]
        flags: CliConfigReorder,
    },
    #[command(about = "Remove process")]
    Remove {
        #[arg(help = "The process name (not necessarily the executable name)")]
        process: String,
    },
}

#[derive(Debug, Args)]
struct CliConfigAdd {
    #[arg(index = 2, help = "The text to display when the process is found")]
    display: String,
    #[arg(
        index = 3,
        help = "URL or image key to display when the process is found"
    )]
    image: String,
    #[arg(
        short = 'i',
        long,
        help = "The index where the program entry should be inserted into the target list"
    )]
    index: Option<u32>,
    #[arg(
        index = 1,
        help = "The process name (not necessarily the executable name)"
    )]
    process: String,
}

#[derive(Debug, Args)]
#[group(multiple = true, required = true)]
struct CliConfigEdit {
    #[arg(short = 'p', long = "process", help = "The new process name")]
    process_edit: Option<String>,
    #[arg(short = 'd', long, help = "The new display text")]
    display: Option<String>,
    #[arg(short = 'i', long, help = "The new image URL/key")]
    image: Option<String>,
}

#[derive(Debug, Args)]
#[group(required = true)]
struct CliConfigReorder {
    #[arg(short = 'i', long, help = "Increase the priority of the process")]
    increase: bool,
    #[arg(short = 'd', long, help = "Decrease the priority of the process")]
    decrease: bool,
    #[arg(
        short = 's',
        long,
        help = "Set the priority of the process to a specific index. Highest priority is 0"
    )]
    set: Option<u32>,
}

enum ConfigReorderOperation {
    Increase,
    Decrease,
    Set(u32),
}

impl From<CliConfigReorder> for ConfigReorderOperation {
    fn from(value: CliConfigReorder) -> Self {
        match value {
            CliConfigReorder {
                increase: true,
                decrease: false,
                set: None,
            } => ConfigReorderOperation::Increase,
            CliConfigReorder {
                increase: false,
                decrease: true,
                set: None,
            } => ConfigReorderOperation::Decrease,
            CliConfigReorder {
                increase: false,
                decrease: false,
                set: Some(index),
            } => ConfigReorderOperation::Set(index),
            _ => unreachable!("Only one operation flag should ever be active"),
        }
    }
}
