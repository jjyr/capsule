use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use ckb_capsule::checker::Checker;
use ckb_capsule::config::{self, Contract, TemplateType};
use ckb_capsule::config_manipulate::{append_contract, Document};
use ckb_capsule::debugger;
use ckb_capsule::deployment::manage::{DeployOption, Manage as DeployManage};
use ckb_capsule::generator::new_project;
use ckb_capsule::project_context::{
    read_config_file, write_config_file, BuildConfig, BuildEnv, Context, DeployEnv, CONFIG_FILE,
};
use ckb_capsule::recipe::get_recipe;
use ckb_capsule::signal;
use ckb_capsule::tester::Tester;
use ckb_capsule::tool::multisig;
use ckb_capsule::version::version_string;
use ckb_capsule::wallet::cli_types::HumanCapacity;
use ckb_capsule::wallet::{Address, Wallet, DEFAULT_CKB_CLI_BIN_NAME, DEFAULT_CKB_RPC_URL};
use ckb_tool::ckb_types::{core::Capacity, packed::Transaction, prelude::*};

use clap::{App, AppSettings, Arg, SubCommand};

const DEBUGGER_MAX_CYCLES: u64 = 70_000_000u64;
const TEMPLATES_NAMES: &[&str] = &["rust", "c", "c-sharedlib"];

fn append_contract_to_config(context: &Context, contract: &Contract) -> Result<()> {
    println!("Rewrite ckb_capsule.toml");
    let mut config_path = context.project_path.clone();
    config_path.push(CONFIG_FILE);
    let config_content = read_config_file(&config_path)?;
    let mut doc = config_content.parse::<Document>()?;
    append_contract(&mut doc, contract.name.to_string(), contract.template_type)?;
    write_config_file(&config_path, doc.to_string())?;
    Ok(())
}

fn select_contracts(context: &Context, names: &[&str]) -> Vec<Contract> {
    context
        .config
        .contracts
        .iter()
        .filter(|c| names.is_empty() || names.contains(&c.name.as_str()))
        .cloned()
        .collect()
}

fn group_contracts_by_type(contracts: Vec<Contract>) -> HashMap<TemplateType, Vec<Contract>> {
    let mut contracts_by_type = HashMap::default();
    for c in contracts {
        contracts_by_type
            .entry(c.template_type)
            .or_insert(Vec::new())
            .push(c.clone());
    }
    contracts_by_type
}

fn run_cli() -> Result<()> {
    env_logger::init();

    let version = version_string();
    let default_max_cycles_str = format!("{}", DEBUGGER_MAX_CYCLES);

    let contract_args = [
        Arg::with_name("name")
            .help("project name")
            .index(1)
            .required(true)
            .takes_value(true),
        Arg::with_name("template")
            .long("template")
            .help("language template")
            .possible_values(TEMPLATES_NAMES)
            .default_value(TEMPLATES_NAMES[0])
            .takes_value(true),
    ];

    let mut app = App::new("Capsule")
        .setting(AppSettings::ArgRequiredElseHelp)
        .version(version.as_str())
        .author("Nervos Developer Tools Team")
        .about("Capsule CKB contract scaffold")
        .subcommand(SubCommand::with_name("check").about("Check environment and dependencies").display_order(0))
        .subcommand(SubCommand::with_name("new").about("Create a new project").args(&contract_args).display_order(1))
        .subcommand(SubCommand::with_name("new-contract").about("Create a new contract").args(&contract_args).display_order(2))
        .subcommand(SubCommand::with_name("build").about("Build contracts").arg(Arg::with_name("name").short("n").long("name").multiple(true).takes_value(true).help("contract name")).arg(
                    Arg::with_name("release").long("release").help("Build contracts in release mode.")
        ).arg(Arg::with_name("debug-output").long("debug-output").help("Always enable debugging output")).display_order(3))
        .subcommand(SubCommand::with_name("run").about("Run command in contract build image").usage("ckb_capsule run --name <name> 'echo list contract dir: && ls'")
        .args(&[Arg::with_name("name").short("n").long("name").required(true).takes_value(true).help("contract name"),
                Arg::with_name("cmd").required(true).multiple(true).help("command to run")])
        .display_order(4))
        .subcommand(SubCommand::with_name("test").about("Run tests").arg(
                    Arg::with_name("release").long("release").help("Test release mode contracts.")
        ).display_order(5))
        .subcommand(
            SubCommand::with_name("deploy")
                .about("Deploy contracts, edit deployment.toml to custodian deployment recipe.")
                .args(&[
                    Arg::with_name("address").long("address").help(
                        "Denote which address provides cells",
                    ).required(true).takes_value(true),
                    Arg::with_name("fee").long("fee").help(
                        "Per transaction's fee, deployment may involve more than one transaction.",
                    ).default_value("0.0001").takes_value(true),
                    Arg::with_name("env").long("env").help("Deployment environment.")
                    .possible_values(&["dev", "production"]).default_value("dev").takes_value(true),
                    Arg::with_name("migrate")
                        .long("migrate")
                        .help("Use previously deployed cells as inputs.").possible_values(&["on", "off"]).default_value("on").takes_value(true),
                    Arg::with_name("api")
                        .long("api")
                        .help("CKB RPC url").default_value(DEFAULT_CKB_RPC_URL).takes_value(true),
                    Arg::with_name("ckb-cli")
                        .long("ckb-cli")
                        .help("CKB cli binary").default_value(DEFAULT_CKB_CLI_BIN_NAME).takes_value(true),
                    Arg::with_name("output")
                        .long("output")
                        .help("write deployment txs as json format to output path").takes_value(true),
                ]).display_order(6),
        )
        .subcommand(
            SubCommand::with_name("tool")
                .about("Tools for develop or deployment contracts.")
                .subcommand(
                    SubCommand::with_name("multisig").about("multisig tools")
                    .subcommand(SubCommand::with_name("calculate-message").about("calculate message for multisig tx")
                    .arg(Arg::with_name("config").long("config").short("c").help("multisig tx configuation").takes_value(true).required(true))
                )
                    .subcommand(
                        SubCommand::with_name("add-signatures").about("add signatures on multisig tx").arg(Arg::with_name("output").help("output path").long("output").short("o").takes_value(true))
                    .arg(Arg::with_name("config").long("config").short("c").help("multisig tx configuation").takes_value(true).required(true))
                    )
                    .subcommand(
                        SubCommand::with_name("new-config").about("generate a multisig tx configuration example").arg(Arg::with_name("output").help("output path").long("output").short("o").takes_value(true))
                    .arg(Arg::with_name("output").long("output").short("o").help("multisig tx configuation output path").takes_value(true).required(true))
                    )
            )
            .subcommand(SubCommand::with_name("send-tx").about("send raw tx to CKB node").arg(Arg::with_name("tx").long("tx").help("raw transaction path").takes_value(true).required(true)).arg(
                    Arg::with_name("api")
                        .long("api")
                        .help("CKB RPC url").default_value(DEFAULT_CKB_RPC_URL).takes_value(true),
                    )
            ).display_order(7))
        .subcommand(SubCommand::with_name("clean").about("Remove contracts targets and binaries").arg(Arg::with_name("name").short("n").long("name").multiple(true).takes_value(true).help("contract name"))
        .display_order(8))
        .subcommand(
            SubCommand::with_name("debugger")
            .about("CKB debugger")
            .subcommand(
                SubCommand::with_name("gen-template")
                .about("Generate transaction debugging template")
                .args(&[
                    Arg::with_name("name").long("name").short("n").help(
                        "contract name",
                    ).required(true).takes_value(true),
                    Arg::with_name("output-file")
                        .long("output-file")
                        .short("o")
                        .help("Output file path").required(true).takes_value(true),
                ])
            )
            .subcommand(
                SubCommand::with_name("start")
                .about("Start GDB")
                .args(&[
                    Arg::with_name("template-file")
                        .long("template-file")
                        .short("f")
                        .help("Transaction debugging template file")
                        .required(true)
                        .takes_value(true),
                    Arg::with_name("name")
                        .short("n")
                        .long("name")
                        .required(true)
                        .takes_value(true)
                        .help("contract name"),
                    Arg::with_name("release").long("release").help("Debugging release contract"),
                    Arg::with_name("script-group-type")
                        .long("script-group-type")
                        .possible_values(&["type", "lock"])
                        .help("Script type")
                        .required(true)
                        .takes_value(true),
                    Arg::with_name("cell-index")
                        .long("cell-index")
                        .required(true)
                        .help("index of the cell")
                        .takes_value(true),
                    Arg::with_name("cell-type")
                        .long("cell-type")
                        .required(true)
                        .possible_values(&["input", "output"])
                        .help("cell type")
                        .takes_value(true),
                    Arg::with_name("max-cycles")
                        .long("max-cycles")
                        .default_value(&default_max_cycles_str)
                        .help("Max cycles")
                        .takes_value(true),
                    Arg::with_name("listen")
                        .long("listen")
                        .short("l")
                        .help("GDB server listening port")
                        .default_value("8000").required(true).takes_value(true),
                    Arg::with_name("only-server").long("only-server").help("Only start debugger server"),
                ])
            )
                .display_order(9),
        );

    let signal = signal::Signal::setup();

    let help_str = {
        let mut buf = Vec::new();
        app.write_long_help(&mut buf)?;
        String::from_utf8(buf)?
    };

    let invalid_command = |command: &str| {
        eprintln!("unrecognize command '{}'", command);
        eprintln!("{}", help_str);
        exit(1)
    };

    let matches = app.get_matches();
    match matches.subcommand() {
        ("check", _args) => {
            Checker::build()?.print_report();
        }
        ("new", Some(args)) => {
            let mut name = args
                .value_of("name")
                .expect("name")
                .trim()
                .trim_end_matches("/")
                .to_string();
            let template_type: TemplateType =
                args.value_of("template").expect("template").parse()?;
            let mut path = PathBuf::new();
            if let Some(index) = name.rfind("/") {
                path.push(&name[..index]);
                name = name[index + 1..].to_string();
            } else {
                path.push(env::current_dir()?);
            }
            let project_path = new_project(name.to_string(), path, &signal)?;
            let context = Context::load_from_path(&project_path)?;
            let c = Contract {
                name,
                template_type,
            };
            get_recipe(context.clone(), c.template_type)?.create_contract(&c, true, &signal)?;
            append_contract_to_config(&context, &c)?;
            println!("Done");
        }
        ("new-contract", Some(args)) => {
            let context = Context::load()?;
            let name = args.value_of("name").expect("name").trim().to_string();
            let template_type: TemplateType =
                args.value_of("template").expect("template").parse()?;
            let contract = Contract {
                name,
                template_type,
            };
            let recipe = get_recipe(context.clone(), contract.template_type)?;
            if recipe.exists(&contract.name) {
                return Err(anyhow!("contract '{}' is already exists", contract.name));
            }
            recipe.create_contract(&contract, true, &signal)?;
            append_contract_to_config(&context, &contract)?;
            println!("Done");
        }
        ("build", Some(args)) => {
            let context = Context::load()?;
            let build_names: Vec<&str> = args
                .values_of("name")
                .map(|values| values.collect())
                .unwrap_or_default();
            let build_env: BuildEnv = if args.is_present("release") {
                BuildEnv::Release
            } else {
                BuildEnv::Debug
            };
            let always_debug = args.is_present("debug-output");
            let build_config = BuildConfig {
                build_env,
                always_debug,
            };

            let contracts: Vec<_> = select_contracts(&context, &build_names);
            if contracts.is_empty() {
                println!("Nothing to do");
            } else {
                for contract in contracts {
                    println!("Building contract {}", contract.name);
                    let recipe = get_recipe(context.clone(), contract.template_type)?;
                    recipe.run_build(&contract, build_config, &signal)?;
                }
                println!("Done");
            }
        }
        ("clean", Some(args)) => {
            let context = Context::load()?;
            let build_names: Vec<&str> = args
                .values_of("name")
                .map(|values| values.collect())
                .unwrap_or_default();
            let contracts: Vec<_> = select_contracts(&context, &build_names);
            if contracts.is_empty() {
                println!("Nothing to do");
            } else {
                let contracts_by_types = group_contracts_by_type(contracts);
                for (template_type, contracts) in contracts_by_types {
                    get_recipe(context.clone(), template_type)?.clean(&contracts, &signal)?;
                }
                println!("Done");
            }
        }
        ("run", Some(args)) => {
            let context = Context::load()?;
            let name = args.value_of("name").expect("name");
            let cmd = args
                .values_of("cmd")
                .expect("cmd")
                .collect::<Vec<&str>>()
                .join(" ");
            let contract = match context.config.contracts.iter().find(|c| name == c.name) {
                Some(c) => c.clone(),
                None => return Err(anyhow!("can't find contract '{}'", name)),
            };
            get_recipe(context, contract.template_type)?.run(&contract, cmd, &signal)?;
        }
        ("test", Some(args)) => {
            let context = Context::load()?;
            let build_env: BuildEnv = if args.is_present("release") {
                BuildEnv::Release
            } else {
                BuildEnv::Debug
            };
            Tester::run(&context, build_env, &signal)?;
        }
        ("deploy", Some(args)) => {
            Checker::build()?.check_ckb_cli()?;
            let address = {
                let address_hex = args.value_of("address").expect("address");
                Address::from_str(&address_hex).expect("parse address")
            };
            let context = Context::load()?;
            let ckb_rpc_url = args.value_of("api").expect("api");
            let ckb_cli_bin = args.value_of("ckb-cli").expect("ckb-cli");
            let wallet = Wallet::load(ckb_rpc_url.to_string(), ckb_cli_bin.to_string(), address);
            let deploy_env: DeployEnv = args
                .value_of("env")
                .expect("deploy env")
                .parse()
                .map_err(|err: &str| anyhow!(err))?;
            let migration_dir = context.migrations_path(deploy_env);
            let migrate = args.value_of("migrate").expect("migrate").to_lowercase() == "on";
            let tx_fee = match HumanCapacity::from_str(args.value_of("fee").expect("tx fee")) {
                Ok(tx_fee) => Capacity::shannons(tx_fee.0),
                Err(err) => return Err(anyhow!(err)),
            };
            let output = match args.value_of("output") {
                Some(s) => Some(PathBuf::from_str(s)?),
                None => None,
            };
            let opt = DeployOption {
                migrate,
                tx_fee,
                output,
            };
            DeployManage::new(migration_dir, context.load_deployment()?).deploy(wallet, opt)?;
        }
        ("tool", Some(sub_matches)) => match sub_matches.subcommand() {
            ("multisig", Some(sub_matches)) => match sub_matches.subcommand() {
                ("calculate-message", Some(args)) => {
                    let config = args.value_of("config").unwrap();
                    let config: config::MultisigConfig =
                        toml::from_str(&fs::read_to_string(config)?)?;
                    let config::MultisigConfig {
                        transaction,
                        lock,
                        signatures: _,
                    } = config;
                    let tx: Transaction = transaction.into();
                    let message = multisig::calculate_multisig_tx_message(&tx.into_view(), &lock)?;
                    println!("{}", message);
                }
                ("add-signatures", Some(args)) => {
                    let output_path = args.value_of("output").unwrap();
                    let config = args.value_of("config").unwrap();
                    let config: config::MultisigConfig =
                        toml::from_str(&fs::read_to_string(config)?)?;
                    let config::MultisigConfig {
                        transaction,
                        lock,
                        signatures,
                    } = config;

                    let mut sigs = Vec::with_capacity(signatures.len());
                    for s in signatures {
                        let s = s.into_bytes();
                        if s.len() != ckb_capsule::tool::multisig::SIGNATURE_SIZE {
                            return Err(anyhow!(
                                "expected signature length is {}, but got {}",
                                ckb_capsule::tool::multisig::SIGNATURE_SIZE,
                                s.len()
                            ));
                        }
                        let mut sig = [0u8; ckb_capsule::tool::multisig::SIGNATURE_SIZE];
                        sig.copy_from_slice(&s);
                        sigs.push(sig);
                    }

                    let tx: Transaction = transaction.into();
                    let signed_tx =
                        multisig::put_signatures_on_multisig_tx(&tx.into_view(), &lock, &sigs)?;
                    fs::write(output_path, signed_tx.data().as_bytes())?;
                    println!("Done");
                }

                ("new-config", Some(args)) => {
                    let output = args.value_of("output").unwrap();
                    let config = config::MultisigConfig::default();
                    let content = toml::to_string_pretty(&config)?;
                    fs::write(output, content)?;
                    println!("Done");
                }
                (command, _) => {
                    invalid_command(command);
                }
            },
            ("send-tx", Some(args)) => {
                let ckb_rpc_url = args.value_of("api").expect("api");
                let client = ckb_tool::rpc_client::RpcClient::new(ckb_rpc_url);
                let tx_path = args.value_of("tx").expect("tx path");
                let raw_tx = fs::read(tx_path)?;
                let raw_tx = Transaction::new_unchecked(raw_tx.into());
                let tx_hash = client.send_transaction(raw_tx.into());
                println!("Sent tx: {}", tx_hash);
            }
            (command, _) => {
                invalid_command(command);
            }
        },
        ("debugger", Some(sub_matches)) => match sub_matches.subcommand() {
            ("gen-template", Some(args)) => {
                let contract = args.value_of("name").expect("contract name");
                let template_content = debugger::build_template(contract.to_string())?;
                let template_path = args.value_of("output-file").expect("output file");
                fs::write(&template_path, template_content)?;
                println!("Write transaction debugging template to {}", template_path,);
            }
            ("start", Some(args)) => {
                let context = Context::load()?;
                let template_path = args.value_of("template-file").expect("template file");
                let build_env: BuildEnv = if args.is_present("release") {
                    BuildEnv::Release
                } else {
                    BuildEnv::Debug
                };
                let contract_name = args.value_of("name").expect("contract name");
                let script_group_type = args.value_of("script-group-type").unwrap();
                let cell_index: usize = args.value_of("cell-index").unwrap().parse()?;
                let cell_type = args.value_of("cell-type").unwrap();
                let max_cycles: u64 = args.value_of("max-cycles").unwrap().parse()?;
                let listen_port: usize = args
                    .value_of("listen")
                    .unwrap()
                    .parse()
                    .expect("listen port");
                let tty = !args.is_present("only-server");
                debugger::start_debugger(
                    &context,
                    template_path,
                    contract_name,
                    build_env,
                    script_group_type,
                    cell_index,
                    cell_type,
                    max_cycles,
                    listen_port,
                    tty,
                    &signal,
                )?;
            }
            (command, _) => {
                invalid_command(command);
            }
        },
        (command, _) => {
            invalid_command(command);
        }
    }
    Ok(())
}

fn main() {
    let backtrace_level = env::var("RUST_BACKTRACE").unwrap_or("".to_string());
    let enable_backtrace =
        !backtrace_level.is_empty() && backtrace_level.as_str() != "0".to_string();
    match run_cli() {
        Ok(_) => {}
        err if enable_backtrace => {
            err.unwrap();
        }
        Err(err) => {
            eprintln!("error: {}", err);
            exit(-1);
        }
    }
}
