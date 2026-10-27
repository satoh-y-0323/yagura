//! Test fixture standing in for a real vendor CLI's `mcp add` / `mcp remove`
//! / `mcp get` subcommands, plus a `probe` and a `file-read` / `file-replace`
//! pair modelling the tools a foreign-environment (WSL) profile row shells
//! out to. Nothing here talks to a real vendor CLI; every behavior is chosen
//! through argv or an environment variable, all documented below. Placed
//! next to `capture_child` (`src/bin/capture_child.rs`) for the same reason:
//! a launch profile's `[command]` can point straight at it.
//!
//! Positional argv, no parser:
//! - `mcp_cli_stub mcp add <name> [<flag> K=V]... -- <cmd> [<arg>...]`:
//!   registers `<name>` against `<cmd> <arg>...`. Any `K=V`-shaped token
//!   before `--` is captured as an environment entry for the registration;
//!   the flag spelling in front of it (`--env`, `-e`, ...) is never
//!   inspected, mirroring how that flag's own spelling varies by CLI in a
//!   profile's `env_arg`. Exits `0` on success, or `1` with an "already
//!   exists" line on stderr when `<name>` is already registered and
//!   `MCP_CLI_STUB_ON_DUPLICATE` is not `"overwrite"`.
//! - `mcp_cli_stub mcp remove <name>`: unregisters `<name>`. Exits `1` with
//!   a "not found" line on stderr when it was never registered.
//! - `mcp_cli_stub mcp get <name> [--json]`: with `--json`, writes
//!   `{"transport":{"command":..,"args":[..]},"tool_timeout_sec":<float, if
//!   set>}` to stdout; without it, writes the line form (`  Scope: ...` /
//!   `  Command: ...` / `  Args: ...`). An unregistered name writes nothing
//!   to stdout, `No MCP server named '<name>' found.` to stderr, and exits
//!   `1` (the shape the real CLIs use).
//! - `mcp_cli_stub mcp list --json`: writes the registry as a JSON array,
//!   one `{"name":..,"transport":{"command":..,"args":[..]}}` element per
//!   registration, in name order; with `MCP_CLI_STUB_LIST_SHAPE=flat`, one
//!   `{"command":..,"args":[..],"env":{..},"enabled":true,"name":..,"scope":"user"}`
//!   element instead.
//! - `mcp_cli_stub probe <path>`: writes one transformed line for `<path>`
//!   (see [`transform_probe_path`]), standing in for a `wslpath`-style call.
//! - `mcp_cli_stub file-read <path>`: writes `<path>`'s bytes to stdout, or
//!   exits `3` when it does not exist -- the exit code a foreign `files.read`
//!   tool uses for "absent".
//! - `mcp_cli_stub file-replace <path>`: reads stdin to end of file and
//!   writes it to `<path>` (creating it, and its parent directories, when
//!   absent), standing in for a `cat > <path>` replace tool.
//!
//! Environment variables (all optional except where noted):
//! - `MCP_CLI_STUB_STATE_DIR` (required for `mcp add` / `remove` / `get` / `list`): a
//!   directory this process keeps its own registration record in
//!   (`registry.json`), independent of any settings file below -- modelling
//!   a CLI's private registry.
//! - `MCP_CLI_STUB_LIST_SHAPE`: `"nested"` (the default) or `"flat"`, the
//!   element shape `mcp list --json` writes.
//! - `MCP_CLI_STUB_ON_DUPLICATE`: `"refuse"` (the default) or `"overwrite"`,
//!   `mcp add`'s behavior when `<name>` is already registered.
//! - `MCP_CLI_STUB_TOML_TABLE_PATH`: a path (resolved against the current
//!   working directory) to a TOML settings file. When set, `mcp add`
//!   rewrites `[mcp_servers.<name>]` there from scratch -- dropping any
//!   hand-written sibling key, the way a real `mcp add` can -- `mcp remove`
//!   deletes that whole table,
//!   and `mcp get --json` reads `tool_timeout_sec` back from it when
//!   present. Left unset, `add` never touches a TOML file at all: the state
//!   a `toml_set` step with no table to edit needs.
//! - `MCP_CLI_STUB_JSON_CONFIG_PATH`: a path (resolved against the current
//!   working directory) to a JSON settings file
//!   (`{"mcpServers":{"<name>":{"command":..,"args":[..]}}}`), written by
//!   `add` and pruned by `remove`, for a profile whose status is read
//!   directly from a file rather than through this binary.
//! - `MCP_CLI_STUB_FAIL_SUBCOMMAND` / `MCP_CLI_STUB_FAIL_EXIT_CODE` /
//!   `MCP_CLI_STUB_FAIL_STDOUT` / `MCP_CLI_STUB_FAIL_STDERR`: when the
//!   dispatched subcommand's name (`add` / `remove` / `get` / `list` / `probe` /
//!   `file-read` / `file-replace`) equals `MCP_CLI_STUB_FAIL_SUBCOMMAND`,
//!   this process writes the given stdout/stderr text -- which may carry
//!   ANSI escapes or bidi control characters, taken literally -- and exits
//!   with the given code (default `1`) instead of running that subcommand,
//!   touching no state.
//! - `MCP_CLI_STUB_DELAY_MS`: sleeps this many milliseconds, right after
//!   this call's start is timestamped, before doing anything else -- for
//!   observing a timeout or a concurrency limit from outside.
//! - `MCP_CLI_STUB_HOLD_UNTIL`: a path; after any delay above, this call
//!   blocks until a file exists there -- for keeping a call in flight until
//!   a test has observed what happens meanwhile, then releasing it by
//!   creating the file. Gives up after [`HOLD_GUARD`], so a test that fails
//!   before releasing leaves no call behind.
//! - `MCP_CLI_STUB_PROBE_OUTPUT` / `MCP_CLI_STUB_PROBE_EXIT_CODE`: override
//!   `probe`'s default transform and exit code.
//! - `MCP_CLI_STUB_LOG`: a path this process appends one JSON line to per
//!   invocation, `{"argv":[..],"started_at_ms":..,"finished_at_ms":..}`.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One `mcp add`-recorded registration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Registration {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

type Registry = BTreeMap<String, Registration>;

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let started_at_ms = now_ms();
    maybe_delay();
    maybe_hold();
    let exit_code = run(&args)?;
    let finished_at_ms = now_ms();
    append_log(&args, started_at_ms, finished_at_ms)?;
    io::stdout().flush()?;
    io::stderr().flush()?;
    std::process::exit(exit_code);
}

fn run(args: &[String]) -> io::Result<i32> {
    let Some(name) = subcommand_name(args) else {
        eprintln!(
            "usage: mcp_cli_stub <mcp add|mcp remove|mcp get|mcp list|probe|file-read|file-replace> ..."
        );
        return Ok(2);
    };
    if let Some(code) = maybe_inject_failure(name)? {
        return Ok(code);
    }
    match name {
        "add" => run_add(&args[2..]),
        "remove" => run_remove(&args[2..]),
        "get" => run_get(&args[2..]),
        "list" => run_list(),
        "probe" => run_probe(&args[1..]),
        "file-read" => run_file_read(&args[1..]),
        "file-replace" => run_file_replace(&args[1..]),
        other => Err(io::Error::other(format!("unhandled subcommand: {other}"))),
    }
}

/// The dispatched subcommand's name, used both for routing and for matching
/// `MCP_CLI_STUB_FAIL_SUBCOMMAND`.
fn subcommand_name(args: &[String]) -> Option<&'static str> {
    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("mcp"), Some("add")) => Some("add"),
        (Some("mcp"), Some("remove")) => Some("remove"),
        (Some("mcp"), Some("get")) => Some("get"),
        (Some("mcp"), Some("list")) => Some("list"),
        (Some("probe"), _) => Some("probe"),
        (Some("file-read"), _) => Some("file-read"),
        (Some("file-replace"), _) => Some("file-replace"),
        _ => None,
    }
}

// --- failure / delay / call-log injection -----------------------------------

/// `Some(exit_code)` when `MCP_CLI_STUB_FAIL_SUBCOMMAND` names `subcommand`:
/// writes the configured stdout/stderr text and reports the code to use
/// instead of running the subcommand at all.
fn maybe_inject_failure(subcommand: &str) -> io::Result<Option<i32>> {
    let Ok(target) = env::var("MCP_CLI_STUB_FAIL_SUBCOMMAND") else {
        return Ok(None);
    };
    if target != subcommand {
        return Ok(None);
    }
    if let Ok(text) = env::var("MCP_CLI_STUB_FAIL_STDOUT") {
        print!("{text}");
    }
    if let Ok(text) = env::var("MCP_CLI_STUB_FAIL_STDERR") {
        eprint!("{text}");
    }
    let code = env::var("MCP_CLI_STUB_FAIL_EXIT_CODE")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(1);
    Ok(Some(code))
}

fn maybe_delay() {
    if let Some(ms) = env::var("MCP_CLI_STUB_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Hang guard of `MCP_CLI_STUB_HOLD_UNTIL`, far above any test's own reply
/// timeout, so it never decides a test's outcome.
const HOLD_GUARD: Duration = Duration::from_secs(60);

fn maybe_hold() {
    let Some(path) = env::var_os("MCP_CLI_STUB_HOLD_UNTIL").map(PathBuf::from) else {
        return;
    };
    let started = Instant::now();
    while !path.exists() && started.elapsed() < HOLD_GUARD {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn append_log(args: &[String], started_at_ms: u128, finished_at_ms: u128) -> io::Result<()> {
    let Some(path) = env::var_os("MCP_CLI_STUB_LOG") else {
        return Ok(());
    };
    let record = serde_json::json!({
        "argv": args,
        "started_at_ms": started_at_ms,
        "finished_at_ms": finished_at_ms,
    });
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{record}")
}

// --- registry (mcp add / remove / get) --------------------------------------

fn state_dir() -> io::Result<PathBuf> {
    env::var_os("MCP_CLI_STUB_STATE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP_CLI_STUB_STATE_DIR must be set for mcp add/remove/get",
            )
        })
}

fn read_registry() -> io::Result<Registry> {
    let path = state_dir()?.join("registry.json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Registry::new()),
        Err(err) => Err(err),
    }
}

fn write_registry(registry: &Registry) -> io::Result<()> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir)?;
    let body = serde_json::to_vec_pretty(registry).map_err(io::Error::other)?;
    fs::write(dir.join("registry.json"), body)
}

/// The parsed form of `<name> [<flag> K=V]... -- <cmd> [<arg>...]`. A struct
/// rather than a tuple: the tuple form of these four fields trips clippy's
/// `type_complexity` lint.
struct ParsedAdd {
    name: String,
    env: BTreeMap<String, String>,
    command: String,
    args: Vec<String>,
}

/// Splits `<name> [<flag> K=V]... -- <cmd> [<arg>...]` into a [`ParsedAdd`].
fn parse_add_args(args: &[String]) -> Option<ParsedAdd> {
    let name = args.first()?.clone();
    let separator = args.iter().position(|arg| arg == "--")?;
    let mut env = BTreeMap::new();
    for token in &args[1..separator] {
        if let Some((key, value)) = token.split_once('=') {
            env.insert(key.to_string(), value.to_string());
        }
    }
    let tail = &args[separator + 1..];
    let command = tail.first()?.clone();
    let args = tail[1..].to_vec();
    Some(ParsedAdd {
        name,
        env,
        command,
        args,
    })
}

fn run_add(args: &[String]) -> io::Result<i32> {
    let Some(parsed) = parse_add_args(args) else {
        eprintln!("usage: mcp_cli_stub mcp add <name> [<flag> K=V]... -- <cmd> [<arg>...]");
        return Ok(2);
    };
    let ParsedAdd {
        name,
        env,
        command,
        args: rest,
    } = parsed;
    let mut registry = read_registry()?;
    if registry.contains_key(&name) {
        let overwrite = env::var("MCP_CLI_STUB_ON_DUPLICATE")
            .map(|value| value == "overwrite")
            .unwrap_or(false);
        if !overwrite {
            eprintln!("MCP server '{name}' already exists.");
            return Ok(1);
        }
    }
    registry.insert(
        name.clone(),
        Registration {
            command: command.clone(),
            args: rest.clone(),
            env,
        },
    );
    write_registry(&registry)?;
    if let Some(path) = toml_table_path() {
        rewrite_toml_table(&path, &name, &command, &rest)?;
    }
    if let Some(path) = json_config_path() {
        upsert_json_config(&path, &name, &command, &rest)?;
    }
    // Real CLIs confirm a successful add on stdout, so a step's output is
    // never empty for a working add.
    println!("Added MCP server '{name}'.");
    Ok(0)
}

fn run_remove(args: &[String]) -> io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: mcp_cli_stub mcp remove <name>");
        return Ok(2);
    };
    let mut registry = read_registry()?;
    if registry.remove(name).is_none() {
        eprintln!("No MCP server named '{name}' found.");
        return Ok(1);
    }
    write_registry(&registry)?;
    if let Some(path) = toml_table_path() {
        remove_toml_table(&path, name)?;
    }
    if let Some(path) = json_config_path() {
        remove_json_config(&path, name)?;
    }
    Ok(0)
}

fn run_get(args: &[String]) -> io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: mcp_cli_stub mcp get <name> [--json]");
        return Ok(2);
    };
    let json = args.get(1).map(String::as_str) == Some("--json");
    let registry = read_registry()?;
    let Some(registration) = registry.get(name) else {
        eprintln!("No MCP server named '{name}' found.");
        return Ok(1);
    };
    if json {
        let mut body = serde_json::json!({
            "transport": { "command": registration.command, "args": registration.args },
        });
        if let Some(path) = toml_table_path()
            && let Some(tool_timeout_sec) = read_tool_timeout_sec(&path, name)?
        {
            body["tool_timeout_sec"] = serde_json::json!(tool_timeout_sec);
        }
        println!("{body}");
    } else {
        println!("  Scope: User config (available in all your projects)");
        println!("  Command: {}", registration.command);
        println!("  Args: {}", registration.args.join(" "));
    }
    Ok(0)
}

fn run_list() -> io::Result<i32> {
    let registry = read_registry()?;
    let flat = env::var("MCP_CLI_STUB_LIST_SHAPE").is_ok_and(|shape| shape == "flat");
    let entries: Vec<Value> = registry
        .iter()
        .map(|(name, registration)| {
            if flat {
                serde_json::json!({
                    "command": registration.command,
                    "args": registration.args,
                    "env": registration.env,
                    "enabled": true,
                    "name": name,
                    "scope": "user",
                })
            } else {
                serde_json::json!({
                    "name": name,
                    "transport": { "command": registration.command, "args": registration.args },
                })
            }
        })
        .collect();
    println!("{}", Value::Array(entries));
    Ok(0)
}

// --- TOML settings-file edit (mcp add / remove / get --json) ---------------

fn toml_table_path() -> Option<PathBuf> {
    env::var_os("MCP_CLI_STUB_TOML_TABLE_PATH").map(PathBuf::from)
}

fn load_or_empty_toml(path: &Path) -> io::Result<toml_edit::Document> {
    match fs::read_to_string(path) {
        Ok(source) => source
            .parse::<toml_edit::Document>()
            .map_err(io::Error::other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(toml_edit::Document::new()),
        Err(err) => Err(err),
    }
}

fn mcp_servers_table_mut(
    document: &mut toml_edit::Document,
) -> io::Result<&mut dyn toml_edit::TableLike> {
    if document.get("mcp_servers").is_none() {
        document
            .as_table_mut()
            .insert("mcp_servers", toml_edit::table());
    }
    document
        .get_mut("mcp_servers")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| io::Error::other("mcp_servers must be a table"))
}

/// Recreates `[mcp_servers.<name>]` from scratch -- dropping any prior
/// sibling key, the way a real `mcp add` can -- setting only `command` and
/// `args`.
fn rewrite_toml_table(path: &Path, name: &str, command: &str, args: &[String]) -> io::Result<()> {
    let mut document = load_or_empty_toml(path)?;
    {
        let servers = mcp_servers_table_mut(&mut document)?;
        servers.remove(name);
        servers.insert(name, toml_edit::table());
        let entry = servers
            .get_mut(name)
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| io::Error::other("the just-inserted table is missing"))?;
        entry.insert("command", toml_edit::value(command));
        let mut array = toml_edit::Array::new();
        for arg in args {
            array.push(arg.as_str());
        }
        entry.insert("args", toml_edit::value(array));
    }
    ensure_parent_dir(path)?;
    fs::write(path, document.to_string())
}

fn remove_toml_table(path: &Path, name: &str) -> io::Result<()> {
    let mut document = match fs::read_to_string(path) {
        Ok(source) => source
            .parse::<toml_edit::Document>()
            .map_err(io::Error::other)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if let Some(servers) = document
        .get_mut("mcp_servers")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        servers.remove(name);
    }
    fs::write(path, document.to_string())
}

fn read_tool_timeout_sec(path: &Path, name: &str) -> io::Result<Option<f64>> {
    let document = match fs::read_to_string(path) {
        Ok(source) => source
            .parse::<toml_edit::Document>()
            .map_err(io::Error::other)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let value = document
        .get("mcp_servers")
        .and_then(|item| item.get(name))
        .and_then(|item| item.get("tool_timeout_sec"))
        .and_then(toml_edit::Item::as_value);
    Ok(value.and_then(toml_value_as_f64))
}

fn toml_value_as_f64(value: &toml_edit::Value) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64))
}

// --- JSON settings-file write (file-form status) ----------------------------

fn json_config_path() -> Option<PathBuf> {
    env::var_os("MCP_CLI_STUB_JSON_CONFIG_PATH").map(PathBuf::from)
}

fn load_or_empty_json(path: &Path) -> io::Result<Value> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            Ok(serde_json::json!({ "mcpServers": {} }))
        }
        Err(err) => Err(err),
    }
}

fn upsert_json_config(path: &Path, name: &str, command: &str, args: &[String]) -> io::Result<()> {
    let mut root = load_or_empty_json(path)?;
    let servers = root
        .as_object_mut()
        .ok_or_else(|| io::Error::other("the JSON config root must be an object"))?
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| io::Error::other("mcpServers must be an object"))?;
    servers.insert(
        name.to_string(),
        serde_json::json!({ "command": command, "args": args }),
    );
    ensure_parent_dir(path)?;
    fs::write(
        path,
        serde_json::to_vec_pretty(&root).map_err(io::Error::other)?,
    )
}

fn remove_json_config(path: &Path, name: &str) -> io::Result<()> {
    let mut root = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_err(io::Error::other)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove(name);
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&root).map_err(io::Error::other)?,
    )
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

// --- probe / file-read / file-replace ---------------------------------------

fn run_probe(args: &[String]) -> io::Result<i32> {
    let Some(path) = args.first() else {
        eprintln!("usage: mcp_cli_stub probe <path>");
        return Ok(2);
    };
    if let Ok(output) = env::var("MCP_CLI_STUB_PROBE_OUTPUT") {
        println!("{output}");
    } else {
        println!("{}", transform_probe_path(path));
    }
    let exit_code = env::var("MCP_CLI_STUB_PROBE_EXIT_CODE")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    Ok(exit_code)
}

/// A fixed, fixture-only transform -- never a real `wslpath` -- so a caller
/// can tell probe's output apart from an unconverted bridge path: forward
/// slashes for backslashes, marked with a `/probed` prefix.
fn transform_probe_path(path: &str) -> String {
    format!("/probed{}", path.replace('\\', "/"))
}

fn run_file_read(args: &[String]) -> io::Result<i32> {
    let Some(path) = args.first() else {
        eprintln!("usage: mcp_cli_stub file-read <path>");
        return Ok(2);
    };
    match fs::read(path) {
        Ok(bytes) => {
            io::stdout().write_all(&bytes)?;
            Ok(0)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(3),
        Err(err) => Err(err),
    }
}

fn run_file_replace(args: &[String]) -> io::Result<i32> {
    let Some(path) = args.first() else {
        eprintln!("usage: mcp_cli_stub file-replace <path>");
        return Ok(2);
    };
    let mut body = Vec::new();
    io::stdin().read_to_end(&mut body)?;
    ensure_parent_dir(Path::new(path))?;
    fs::write(path, body)?;
    Ok(0)
}
