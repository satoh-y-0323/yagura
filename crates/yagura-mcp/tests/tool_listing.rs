#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// This test observes what a real MCP client receives, so it must start the
// server as an actual child process rather than call into it in-process.
#![allow(clippy::disallowed_methods)]

//! What a real MCP client receives from `tools/list`, not the literal source
//! strings in `main.rs`: some clients silently truncate a long `description`,
//! so anything a caller needs must survive that truncation.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Claude Code silently truncates a tool's `description` past this many
/// characters (observed: `Tool "send_handoff" description truncated from
/// 2461 to 2048 chars`), so anything the caller needs must fit inside it.
const MAX_TOOL_DESCRIPTION_CHARS: usize = 2048;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

const KNOWN_TOOL_NAMES: &[&str] = &[
    "list_windows",
    "get_window_status",
    "send_message",
    "send_keys",
    "open_window",
    "interrupt_window",
    "close_window",
    "wait_for_status",
    "read_output",
    "list_detections",
    "list_profiles",
    "send_handoff",
    "send_chat",
    "broadcast_chat",
    "read_chat",
    "create_plan",
    "update_plan",
    "get_plan",
    "list_plans",
    "delete_plan",
    "list_roles",
    "open_task_window",
    "send_report",
    "wait_for_report",
    "list_tickets",
    "get_ticket",
    "create_ticket",
    "update_ticket",
    "add_ticket_result",
    "set_milestone",
];

/// Kills the spawned `yagura-mcp` process on drop, so a failing assertion
/// (which unwinds past the rest of the test) never leaves it running.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawns the real `yagura-mcp` binary and speaks the stdio JSON-RPC
/// handshake up to `tools/list`, returning the response the client actually
/// receives. The binary never connects to a daemon at startup, so this works
/// with no daemon running.
fn fetch_tools_list() -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_yagura-mcp"))
        .env_remove("YAGURA_WINDOW_ID")
        .env_remove("YAGURA_STATUS_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the yagura-mcp binary");

    let mut stdin = child.stdin.take().expect("child stdin was not piped");
    let stdout = child.stdout.take().expect("child stdout was not piped");
    let _guard = ChildGuard(child);

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    send_line(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    );
    recv_response(&rx, 1);

    send_line(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    send_line(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    );
    recv_response(&rx, 2)
}

fn send_line(stdin: &mut ChildStdin, value: &Value) {
    let mut line = serde_json::to_string(value).expect("serialize a JSON-RPC request");
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .expect("write to child stdin");
    stdin.flush().expect("flush child stdin");
}

/// Reads lines from `rx` until one parses as a JSON-RPC response whose `id`
/// matches, or `RESPONSE_TIMEOUT` elapses -- never blocks forever.
fn recv_response(rx: &mpsc::Receiver<String>, want_id: i64) -> Value {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting {RESPONSE_TIMEOUT:?} for a response with id {want_id}");
        }
        let line = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("timed out waiting for a response with id {want_id}"));
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) == Some(want_id) {
            return value;
        }
    }
}

fn tools_array(response: &Value) -> &Vec<Value> {
    response
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .expect("tools/list response has a result.tools array")
}

fn find_tool<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("tools/list did not return a tool named {name}"))
}

#[test]
fn tools_list_names_every_public_tool() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    let mut actual: Vec<&str> = tools
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .expect("every tool has a name")
        })
        .collect();
    actual.sort_unstable();

    let mut expected: Vec<&str> = KNOWN_TOOL_NAMES.to_vec();
    expected.sort_unstable();

    assert_eq!(
        actual, expected,
        "tools/list returned a different tool name set than expected"
    );
}

#[test]
fn every_tool_description_fits_the_clients_2048_char_limit() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    let oversized: Vec<(String, usize)> = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_string();
            let description = tool.get("description").and_then(Value::as_str)?;
            let len = description.chars().count();
            (len > MAX_TOOL_DESCRIPTION_CHARS).then_some((name, len))
        })
        .collect();

    assert!(
        oversized.is_empty(),
        "tool descriptions exceed the {MAX_TOOL_DESCRIPTION_CHARS}-char client limit: {oversized:?}"
    );
}

#[test]
fn send_handoff_explains_every_argument_in_its_schema() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let send_handoff = find_tool(tools, "send_handoff");

    let properties = send_handoff
        .get("inputSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .expect("send_handoff has inputSchema.properties");

    for key in ["window_id", "profile", "cwd", "name", "reference", "note"] {
        let description = properties
            .get(key)
            .unwrap_or_else(|| panic!("send_handoff schema is missing property {key}"))
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("send_handoff property {key} has no description"));
        assert!(
            !description.is_empty(),
            "send_handoff property {key} has an empty description"
        );
    }
}

/// Guidance a caller cannot act on the tool without, measured against what a
/// truncating client actually delivers rather than against the full string:
/// how to answer a handoff (`reply.window_id`), the reference/note/envelope
/// byte limits, and every delivery outcome the reply can carry, so a caller can
/// tell an envelope the receiving CLI took from one it never will.
///
/// A description that grows loses its tail first, so every sentence added for
/// the delivery outcome has to be paid for by compressing another one, and this
/// test is what says whether that payment was made.
const REQUIRED_DELIVERED_GUIDANCE: &[&str] = &[
    "reply.window_id",
    "512 and 1024 bytes",
    "taken",
    "undelivered_timeout",
    "undelivered_no_delivery",
];

/// Delivery outcomes of a handoff that no longer exist: the daemon no longer types an
/// envelope into a window and watches its composer.
const RETIRED_HANDOFF_OUTCOMES: &[&str] = &["unconfirmed", "not_observed", "not_written"];

#[test]
fn send_handoff_keeps_the_reply_instruction_inside_the_delivered_description() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let send_handoff = find_tool(tools, "send_handoff");

    let description = send_handoff
        .get("description")
        .and_then(Value::as_str)
        .expect("send_handoff has a description");
    let delivered: String = description
        .chars()
        .take(MAX_TOOL_DESCRIPTION_CHARS)
        .collect();

    let missing: Vec<&str> = REQUIRED_DELIVERED_GUIDANCE
        .iter()
        .copied()
        .filter(|needle| !delivered.contains(needle))
        .collect();

    assert!(
        missing.is_empty(),
        "guidance the caller needs falls outside the first {MAX_TOOL_DESCRIPTION_CHARS} chars \
         a truncating client actually delivers: {missing:?}"
    );
    for retired in RETIRED_HANDOFF_OUTCOMES {
        assert!(
            !description.contains(retired),
            "send_handoff description still names the retired outcome {retired:?}: \
             {description:?}"
        );
    }
}

/// Both tools hand a request to a CLI at launch, which a Windows `.cmd` / `.bat` program
/// cannot carry: a caller has to learn from the delivered description that such a profile
/// is refused, and as which code.
#[test]
fn the_tools_that_hand_a_request_at_launch_say_a_windows_shim_profile_is_refused() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    for tool_name in ["send_handoff", "open_task_window"] {
        let delivered: String = find_tool(tools, tool_name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{tool_name} has a description"))
            .chars()
            .take(MAX_TOOL_DESCRIPTION_CHARS)
            .collect();
        assert!(
            delivered.contains(".cmd / .bat") && delivered.contains("unsupported"),
            "{tool_name} must say a profile whose program is a Windows .cmd / .bat file is \
             refused as unsupported, within what a truncating client delivers: {delivered:?}"
        );
    }
}

/// `open_window` and `send_handoff` both accept `model` and `effort` only
/// alongside a launch profile; the alias vocabulary they draw from lives on
/// the profile, not on the tool itself, so each property's own description
/// has to send the caller to `list_profiles` to learn it.
#[test]
fn open_window_and_send_handoff_schemas_document_model_and_effort_via_list_profiles() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for tool_name in ["open_window", "send_handoff"] {
        let tool = find_tool(tools, tool_name);
        let properties = tool
            .get("inputSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{tool_name} has inputSchema.properties"));

        for key in ["model", "effort"] {
            let description = properties
                .get(key)
                .unwrap_or_else(|| panic!("{tool_name} schema is missing property {key}"))
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{tool_name} property {key} has no description"));
            assert!(
                !description.is_empty(),
                "{tool_name} property {key} has an empty description"
            );
            assert!(
                description.contains("list_profiles"),
                "{tool_name} property {key} description must point the caller at list_profiles to learn the available aliases, got: {description:?}"
            );
        }
    }
}

/// `open_window` names the two launch options it accepts, and `list_profiles`
/// names the axes an entry's `model` / `effort` keys report, so a caller
/// reading either tool's own description sees both words.
#[test]
fn open_window_and_list_profiles_descriptions_mention_model_and_effort() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for tool_name in ["open_window", "list_profiles"] {
        let tool = find_tool(tools, tool_name);
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{tool_name} has a description"));
        for word in ["model", "effort"] {
            assert!(
                description.contains(word),
                "{tool_name} description must mention {word:?}, got: {description:?}"
            );
        }
    }
}

#[test]
fn the_three_new_chat_tool_descriptions_fit_the_clients_2048_char_limit() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in ["send_chat", "broadcast_chat", "read_chat"] {
        let tool = find_tool(tools, name);
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name} has a description"));
        let len = description.chars().count();
        assert!(
            len <= MAX_TOOL_DESCRIPTION_CHARS,
            "{name} description is {len} chars, exceeding the {MAX_TOOL_DESCRIPTION_CHARS}-char \
             client limit"
        );
    }
}

/// Guidance a caller cannot page through the room's history without: that
/// `next_after_seq` from one reply is the `after_seq` to send on the next
/// call, that a message mentioning `@user` asks a human to decide something,
/// that the room's current participants come back as `roster`, and every state a
/// delivery record can be in.
const READ_CHAT_REQUIRED_GUIDANCE: &[&str] = &[
    "next_after_seq",
    "back as after_seq",
    "@user",
    "asks a human to decide",
    "roster",
    "pending",
    "taken",
    "undelivered_unknown",
    "undelivered_halted",
    "undelivered_exhausted",
    "undelivered_restart",
];

/// Delivery states of a chat post that no longer exist: they described what a
/// window's composer showed after the daemon typed into it.
const RETIRED_CHAT_DELIVERY_STATES: &[&str] =
    &["confirmed", "not_observed", "not_written", "write_failed"];

#[test]
fn read_chat_description_documents_after_seq_persistence_at_mentions_and_roster() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let read_chat = find_tool(tools, "read_chat");

    let description = read_chat
        .get("description")
        .and_then(Value::as_str)
        .expect("read_chat has a description");

    let missing: Vec<&str> = READ_CHAT_REQUIRED_GUIDANCE
        .iter()
        .copied()
        .filter(|needle| !description.contains(needle))
        .collect();

    assert!(
        missing.is_empty(),
        "read_chat description is missing required guidance: {missing:?}, got: {description:?}"
    );
    for retired in RETIRED_CHAT_DELIVERY_STATES {
        assert!(
            !description.contains(retired),
            "read_chat description still names the retired state {retired:?}: {description:?}"
        );
    }
}

/// Guidance a caller needs to address a chat post: the mention syntax for a
/// bare name, a name containing a space, and the reserved `@user` mention,
/// plus the rule that a window which is not a chat participant (a plain
/// shell, for instance) never receives one.
const SEND_CHAT_REQUIRED_GUIDANCE: &[&str] = &[
    "@name",
    "@\"a b\"",
    "@user",
    "windows that are not participants",
    "plain shell",
    "does not receive it",
];

#[test]
fn send_chat_description_documents_mention_syntax_and_the_participants_only_rule() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let send_chat = find_tool(tools, "send_chat");

    let description = send_chat
        .get("description")
        .and_then(Value::as_str)
        .expect("send_chat has a description");

    let missing: Vec<&str> = SEND_CHAT_REQUIRED_GUIDANCE
        .iter()
        .copied()
        .filter(|needle| !description.contains(needle))
        .collect();

    assert!(
        missing.is_empty(),
        "send_chat description is missing required guidance: {missing:?}, got: {description:?}"
    );
}

const PLAN_TOOL_NAMES: &[&str] = &[
    "create_plan",
    "update_plan",
    "get_plan",
    "list_plans",
    "delete_plan",
    "list_roles",
    "open_task_window",
];

/// The part of `name`'s description a truncating client actually delivers,
/// lowercased so guidance is matched regardless of sentence position.
fn delivered_description(tools: &[Value], name: &str) -> String {
    find_tool(tools, name)
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name} has a description"))
        .chars()
        .take(MAX_TOOL_DESCRIPTION_CHARS)
        .collect::<String>()
        .to_lowercase()
}

#[test]
fn the_seven_plan_tool_descriptions_fit_the_clients_2048_char_limit() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in PLAN_TOOL_NAMES {
        let description = find_tool(tools, name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name} has a description"));
        let len = description.chars().count();
        assert!(
            len <= MAX_TOOL_DESCRIPTION_CHARS,
            "{name} description is {len} chars, exceeding the {MAX_TOOL_DESCRIPTION_CHARS}-char \
             client limit"
        );
    }
}

/// Each plan tool's schema names the arguments the tool takes and marks as
/// required exactly the ones it refuses to run without.
const PLAN_TOOL_SCHEMAS: &[(&str, &[&str], &[&str])] = &[
    (
        "create_plan",
        &["title", "tasks", "ticket"],
        &["title", "tasks"],
    ),
    (
        "update_plan",
        &["plan_id", "title", "tasks"],
        &["plan_id", "tasks"],
    ),
    ("get_plan", &["plan_id", "task_id"], &["plan_id"]),
    ("list_plans", &[], &[]),
    ("delete_plan", &["plan_id"], &["plan_id"]),
    ("list_roles", &[], &[]),
    (
        "open_task_window",
        &["plan_id", "task_id", "name"],
        &["plan_id", "task_id"],
    ),
];

#[test]
fn plan_tool_schemas_name_every_argument_and_require_exactly_the_mandatory_ones() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, properties, required) in PLAN_TOOL_SCHEMAS {
        assert_schema_names_and_requires(tools, name, properties, required);
    }
}

/// `open_window` launches from either a `command` or a `profile`, which a
/// schema `required` list cannot express, so it requires none of its
/// arguments: listing `command` there would make a schema-validating client
/// reject every profile launch. `list_profiles` takes no argument at all.
const LAUNCH_TOOL_SCHEMAS: &[(&str, &[&str], &[&str])] = &[
    (
        "open_window",
        &[
            "command",
            "cwd",
            "name",
            "vendor",
            "detection",
            "profile",
            "model",
            "effort",
        ],
        &[],
    ),
    ("list_profiles", &[], &[]),
];

#[test]
fn launch_tool_schemas_name_every_argument_and_require_none() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, properties, required) in LAUNCH_TOOL_SCHEMAS {
        assert_schema_names_and_requires(tools, name, properties, required);
    }
}

/// Asserts that tool `name`'s delivered `inputSchema` declares exactly
/// `properties` and marks exactly `required` as required, order aside.
fn assert_schema_names_and_requires(
    tools: &[Value],
    name: &str,
    properties: &[&str],
    required: &[&str],
) {
    let schema = find_tool(tools, name)
        .get("inputSchema")
        .unwrap_or_else(|| panic!("{name} has an inputSchema"));

    let mut actual_properties: Vec<&str> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    actual_properties.sort_unstable();
    let mut expected_properties = properties.to_vec();
    expected_properties.sort_unstable();
    assert_eq!(
        actual_properties, expected_properties,
        "{name} schema names a different argument set"
    );

    let mut actual_required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    actual_required.sort_unstable();
    let mut expected_required = required.to_vec();
    expected_required.sort_unstable();
    assert_eq!(
        actual_required, expected_required,
        "{name} schema requires a different argument set"
    );
}

/// The fields a caller writes for each task of `create_plan` and
/// `update_plan`, which the schema has to spell out for the caller to send
/// them at all.
const PLAN_TASK_FIELDS: &[&str] = &[
    "task_id",
    "title",
    "role",
    "profile",
    "model",
    "effort",
    "depends_on",
    "writes",
];

#[test]
fn create_plan_and_update_plan_schemas_spell_out_every_task_field() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in ["create_plan", "update_plan"] {
        let task_properties = find_tool(tools, name)
            .get("inputSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.get("tasks"))
            .and_then(|tasks| tasks.get("items"))
            .and_then(|items| items.get("properties"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name} schema has tasks.items.properties"));

        let missing: Vec<&str> = PLAN_TASK_FIELDS
            .iter()
            .copied()
            .filter(|field| !task_properties.contains_key(*field))
            .collect();
        assert!(
            missing.is_empty(),
            "{name} schema leaves task fields undocumented: {missing:?}"
        );
    }
}

/// The resolution order of a task's assignment, in the vocabulary the
/// replies' `source` field uses.
const ASSIGNMENT_PRECEDENCE: &str = "user > parent > role";

/// Guidance each plan tool's delivered description must carry, as groups of
/// which at least one alternative must appear (matched case-insensitively):
/// that waves are derived from `depends_on`, that unordered tasks writing the
/// same file are refused, who may write a plan (its owner's window, while it
/// is alive, the owner being replaceable from the GUI), who may read one (the
/// owner and windows bound to its tasks), the codes a caller outside those
/// sets gets, and the assignment precedence.
const PLAN_TOOL_REQUIRED_GUIDANCE: &[(&str, &[&[&str]])] = &[
    (
        "create_plan",
        &[
            &["depends_on"],
            &["wave"],
            &["same file", "same path"],
            &["reject", "refuse"],
            &["owner"],
            &["gui"],
            &[ASSIGNMENT_PRECEDENCE],
        ],
    ),
    (
        "update_plan",
        &[
            &["owner"],
            &["alive", "living"],
            &["gui"],
            &["not_plan_owner"],
            &["plan_not_found"],
        ],
    ),
    (
        "get_plan",
        &[&["owner"], &["bound", "opened for"], &["plan_not_found"]],
    ),
    ("list_plans", &[&["owner"], &["bound", "opened for"]]),
    (
        "delete_plan",
        &[
            &["owner"],
            &["alive", "living"],
            &["not_plan_owner"],
            &["plan_not_found"],
        ],
    ),
    ("list_roles", &[&["role"], &["profile"]]),
    (
        "open_task_window",
        &[
            &["owner"],
            &["alive", "living"],
            &["not_plan_owner"],
            &["plan_not_found"],
            &[ASSIGNMENT_PRECEDENCE],
        ],
    ),
];

#[test]
fn plan_tool_descriptions_explain_waves_conflicts_ownership_visibility_and_precedence() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, groups) in PLAN_TOOL_REQUIRED_GUIDANCE {
        let description = delivered_description(tools, name);
        let missing: Vec<&[&str]> = groups
            .iter()
            .copied()
            .filter(|alternatives| {
                !alternatives
                    .iter()
                    .any(|needle| description.contains(&needle.to_lowercase()))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "{name} description is missing required guidance {missing:?}, got: {description:?}"
        );
    }
}

/// Phrasings that would pin the plan's writer to the window that created it,
/// which stops being true once the user names another window the owner.
const OWNER_IS_ONLY_CREATOR_PHRASES: &[&str] = &[
    "only the window that created",
    "only by the window that created",
    "only the creating window",
    "only by the creating window",
    "only the creator",
    "only its creator",
    "only by its creator",
    "creator only",
];

#[test]
fn no_plan_tool_description_equates_the_owner_with_the_creating_window() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in PLAN_TOOL_NAMES {
        let description = delivered_description(tools, name);
        let found: Vec<&str> = OWNER_IS_ONLY_CREATOR_PHRASES
            .iter()
            .copied()
            .filter(|phrase| description.contains(phrase))
            .collect();
        assert!(
            found.is_empty(),
            "{name} description ties the owner to the creating window: {found:?}"
        );
    }
}

/// yagura carries no workflow of its own, so a plan tool must not present the
/// names of workflow stages as defaults a caller should follow.
const WORKFLOW_STAGE_WORD_PREFIXES: &[&str] = &["design", "implement", "review"];
const WORKFLOW_STAGE_WORDS: &[&str] = &["tester", "testing"];

#[test]
fn no_plan_or_report_tool_description_names_workflow_stages() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in PLAN_TOOL_NAMES.iter().chain(REPORT_TOOL_NAMES) {
        let description = delivered_description(tools, name);
        let found: Vec<&str> = description
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| {
                WORKFLOW_STAGE_WORD_PREFIXES
                    .iter()
                    .any(|prefix| word.starts_with(prefix))
                    || WORKFLOW_STAGE_WORDS.contains(word)
            })
            .collect();
        assert!(
            found.is_empty(),
            "{name} description names workflow stages: {found:?}"
        );
    }
}

const REPORT_TOOL_NAMES: &[&str] = &["send_report", "wait_for_report"];

#[test]
fn the_two_report_tool_descriptions_fit_the_clients_2048_char_limit() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in REPORT_TOOL_NAMES {
        let description = find_tool(tools, name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name} has a description"));
        let len = description.chars().count();
        assert!(
            len <= MAX_TOOL_DESCRIPTION_CHARS,
            "{name} description is {len} chars, exceeding the {MAX_TOOL_DESCRIPTION_CHARS}-char \
             client limit"
        );
    }
}

/// Each report tool's schema names the arguments the tool takes and marks as
/// required exactly the ones it refuses to run without.
const REPORT_TOOL_SCHEMAS: &[(&str, &[&str], &[&str])] = &[
    (
        "send_report",
        &["outcome", "summary", "path"],
        &["outcome", "summary"],
    ),
    ("wait_for_report", &["timeout_sec"], &["timeout_sec"]),
];

#[test]
fn report_tool_schemas_name_every_argument_and_require_exactly_the_mandatory_ones() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, properties, required) in REPORT_TOOL_SCHEMAS {
        let schema = find_tool(tools, name)
            .get("inputSchema")
            .unwrap_or_else(|| panic!("{name} has an inputSchema"));

        let mut actual_properties: Vec<&str> = schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        actual_properties.sort_unstable();
        let mut expected_properties = properties.to_vec();
        expected_properties.sort_unstable();
        assert_eq!(
            actual_properties, expected_properties,
            "{name} schema names a different argument set"
        );

        let mut actual_required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        actual_required.sort_unstable();
        let mut expected_required = required.to_vec();
        expected_required.sort_unstable();
        assert_eq!(
            actual_required, expected_required,
            "{name} schema requires a different argument set"
        );
    }
}

/// Returns the groups of which no alternative appears in `description`.
fn missing_groups<'a>(description: &str, groups: &[&'a [&'a str]]) -> Vec<&'a [&'a str]> {
    groups
        .iter()
        .copied()
        .filter(|alternatives| {
            !alternatives
                .iter()
                .any(|needle| description.contains(&needle.to_lowercase()))
        })
        .collect()
}

/// The guidance a window running handed-over work needs from `send_report`:
/// call it once when the work is finished, put a long result in a file and
/// pass its `path`, and consult over chat rather than stopping at its own
/// terminal's interactive question. Both chat tools are named, because the
/// chat is a room for every participant, not a line to one window.
const SEND_REPORT_REQUIRED_GUIDANCE: &[&[&str]] = &[
    &["once"],
    &["finish", "done", "complete"],
    &["long", "large", "lengthy"],
    &["file"],
    &["path"],
    &["interactive"],
    &["send_chat"],
    &["broadcast_chat"],
];

#[test]
fn send_report_description_says_when_to_call_it_where_long_results_go_and_to_use_chat() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "send_report");

    let missing = missing_groups(&description, SEND_REPORT_REQUIRED_GUIDANCE);
    assert!(
        missing.is_empty(),
        "send_report description is missing required guidance {missing:?}, got: {description:?}"
    );
}

/// The notice line the daemon types into a waiting window when reports are
/// pending; `wait_for_report` tells the reader to call it on seeing this.
const REPORT_NOTICE_PREFIX: &str = "[yagura report]";

const WAIT_FOR_REPORT_REQUIRED_GUIDANCE: &[&[&str]] = &[
    &["wait"],
    &["report"],
    &["child", "you opened"],
    &[REPORT_NOTICE_PREFIX],
    &["timeout_sec"],
    &["without waiting", "immediately", "at once", "right away"],
];

#[test]
fn wait_for_report_description_says_it_waits_for_reports_on_the_notice_and_zero_returns_at_once() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "wait_for_report");

    let missing = missing_groups(&description, WAIT_FOR_REPORT_REQUIRED_GUIDANCE);
    assert!(
        missing.is_empty(),
        "wait_for_report description is missing required guidance {missing:?}, got: {description:?}"
    );
    let names_zero = description
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "0" || word == "zero");
    assert!(
        names_zero,
        "wait_for_report description must say what a timeout_sec of 0 does, got: {description:?}"
    );
}

#[test]
fn send_chat_description_lists_at_parent_alongside_at_user() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "send_chat");

    for mention in ["@parent", "@user"] {
        assert!(
            description.contains(mention),
            "send_chat description must name {mention} as a mention it accepts, got: \
             {description:?}"
        );
    }
}

/// `open_task_window` returns without waiting for its notice, so its
/// description points at where the delivery state is read instead.
const OPEN_TASK_WINDOW_NOTICE_GUIDANCE: &[&[&str]] = &[&["instruction_notice"], &["get_plan"]];

/// What each `instruction_notice` state means.
const GET_PLAN_NOTICE_GUIDANCE: &[&[&str]] = &[
    &["instruction_notice"],
    &["pending"],
    &["delivered"],
    &["window_closed"],
];

#[test]
fn open_task_window_description_points_to_the_notice_state_in_get_plan_and_never_to_a_wait() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "open_task_window");

    let missing = missing_groups(&description, OPEN_TASK_WINDOW_NOTICE_GUIDANCE);
    assert!(
        missing.is_empty(),
        "open_task_window description is missing required guidance {missing:?}, got: \
         {description:?}"
    );
    for stale in ["instruction_delivery", "waits for the new window"] {
        assert!(
            !description.contains(stale),
            "open_task_window description still describes the removed wait ({stale:?}), got: \
             {description:?}"
        );
    }
}

/// The notice line the daemon types into a plan owner's window once an
/// approver settles the plan.
const PLAN_NOTICE_PREFIX: &str = "[yagura plan]";

/// A caller that creates a plan or opens its tasks must learn that the tasks
/// stay closed until the plan is approved, and which line tells it so.
const PLAN_REVIEW_GUIDANCE: &[(&str, &[&[&str]])] = &[
    (
        "create_plan",
        &[&["plan_not_approved"], &[PLAN_NOTICE_PREFIX]],
    ),
    (
        "open_task_window",
        &[&["plan_not_approved"], &[PLAN_NOTICE_PREFIX]],
    ),
];

#[test]
fn create_plan_and_open_task_window_descriptions_name_the_review_refusal_and_its_notice() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, groups) in PLAN_REVIEW_GUIDANCE {
        let description = delivered_description(tools, name);
        let missing = missing_groups(&description, groups);
        assert!(
            missing.is_empty(),
            "{name} description is missing required guidance {missing:?}, got: {description:?}"
        );
    }
}

// --- mcp_connected and awaiting are documented -----------------------

/// The two tools that can report `mcp_connected` must explain it, and
/// the explanation must fit inside the client's truncation limit.
const MCP_CONNECTED_TOOLS: &[&str] = &["list_windows", "get_window_status"];

#[test]
fn list_windows_and_get_window_status_descriptions_explain_mcp_connected() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in MCP_CONNECTED_TOOLS {
        let description = find_tool(tools, name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name} has a description"));
        assert!(
            description.contains("mcp_connected"),
            "{name} description must explain mcp_connected, got: {description:?}"
        );
        let len = description.chars().count();
        assert!(
            len <= MAX_TOOL_DESCRIPTION_CHARS,
            "{name} description is {len} chars, exceeding the {MAX_TOOL_DESCRIPTION_CHARS}-char \
             client limit"
        );
    }
}

#[test]
fn wait_for_report_description_explains_awaiting() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "wait_for_report");

    assert!(
        description.contains("awaiting"),
        "wait_for_report description must explain the awaiting key, got: {description:?}"
    );
}

#[test]
fn get_plan_description_explains_each_instruction_notice_state() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "get_plan");

    let missing = missing_groups(&description, GET_PLAN_NOTICE_GUIDANCE);
    assert!(
        missing.is_empty(),
        "get_plan description is missing required guidance {missing:?}, got: {description:?}"
    );
}

const TICKET_TOOL_NAMES: &[&str] = &[
    "list_tickets",
    "get_ticket",
    "create_ticket",
    "update_ticket",
    "add_ticket_result",
    "set_milestone",
];

#[test]
fn the_six_ticket_tool_descriptions_fit_the_clients_2048_char_limit() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in TICKET_TOOL_NAMES {
        let description = find_tool(tools, name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name} has a description"));
        let len = description.chars().count();
        assert!(
            len <= MAX_TOOL_DESCRIPTION_CHARS,
            "{name} description is {len} chars, exceeding the {MAX_TOOL_DESCRIPTION_CHARS}-char \
             client limit"
        );
    }
}

/// Each ticket tool's schema names the arguments the tool takes and marks as
/// required exactly the ones it refuses to run without. `list_tickets` is
/// checked on its own below.
const TICKET_TOOL_SCHEMAS: &[(&str, &[&str], &[&str])] = &[
    ("get_ticket", &["ticket"], &["ticket"]),
    (
        "create_ticket",
        &["title", "body", "milestone", "state", "fields"],
        &["title", "body"],
    ),
    (
        "update_ticket",
        &["ticket", "title", "body", "milestone", "state", "fields"],
        &["ticket"],
    ),
    (
        "add_ticket_result",
        &["ticket", "text"],
        &["ticket", "text"],
    ),
    (
        "set_milestone",
        &["name", "title", "description", "position"],
        &["name", "title", "position"],
    ),
];

#[test]
fn ticket_tool_schemas_name_every_argument_and_require_exactly_the_mandatory_ones() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for (name, properties, required) in TICKET_TOOL_SCHEMAS {
        assert_schema_names_and_requires(tools, name, properties, required);
    }
}

/// `list_tickets` takes a state and a milestone to narrow by, requires
/// nothing, and never lets a window name the workspace it reads (a window
/// reads only its own). A `limit` is the one argument it may carry besides.
#[test]
fn list_tickets_schema_narrows_by_state_and_milestone_requires_nothing_and_names_no_workspace() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let schema = find_tool(tools, "list_tickets")
        .get("inputSchema")
        .expect("list_tickets has an inputSchema");

    let properties: Vec<&str> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    for wanted in ["state", "milestone"] {
        assert!(
            properties.contains(&wanted),
            "list_tickets must take {wanted}, schema names {properties:?}"
        );
    }
    let unexpected: Vec<&&str> = properties
        .iter()
        .filter(|name| !["state", "milestone", "limit"].contains(*name))
        .collect();
    assert!(
        unexpected.is_empty(),
        "list_tickets names arguments it must not take: {unexpected:?}"
    );

    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(required, 0, "list_tickets requires no argument");
}

/// A ticket is named by its number, so the schema has to say it is an
/// integer; an optional `ticket` on `create_plan` is how a plan is made from
/// one.
#[test]
fn the_ticket_argument_is_an_integer_on_get_ticket_and_create_plan() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in ["get_ticket", "create_plan"] {
        let ticket = find_tool(tools, name)
            .get("inputSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.get("ticket"))
            .unwrap_or_else(|| panic!("{name} schema names a ticket argument"));
        let kind = ticket
            .get("type")
            .unwrap_or_else(|| panic!("{name} ticket schema declares a type: {ticket}"))
            .to_string();
        assert!(
            kind.contains("integer"),
            "{name} ticket must be an integer, schema says {kind}"
        );
    }
}

/// The sentences of `description` (already lowercased by
/// [`delivered_description`]), split at a full stop or a line break.
fn sentences(description: &str) -> Vec<&str> {
    description
        .split(['.', '\n'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

const DATA_NOT_INSTRUCTIONS: &str = "data, not instructions";

/// A ticket's body, a field's value and a result are written by whoever had
/// access to the board, so a reader must treat them as data. The `guide` and
/// the state and field descriptions are the user's own rules for this board,
/// which a reader is meant to follow: they must not be filed under the same
/// warning, or the reader is told to ignore the one place the user speaks.
#[test]
fn get_ticket_and_list_tickets_call_ticket_text_data_but_present_the_guide_as_the_users_rules() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);

    for name in ["get_ticket", "list_tickets"] {
        let description = delivered_description(tools, name);
        let sentences = sentences(&description);
        assert!(
            sentences
                .iter()
                .any(|sentence| sentence.contains(DATA_NOT_INSTRUCTIONS)),
            "{name} description must say ticket text is {DATA_NOT_INSTRUCTIONS}: {description:?}"
        );
        let guide_under_the_warning: Vec<&&str> = sentences
            .iter()
            .filter(|sentence| {
                sentence.contains(DATA_NOT_INSTRUCTIONS) && sentence.contains("guide")
            })
            .collect();
        assert!(
            guide_under_the_warning.is_empty(),
            "{name} must not file the guide under {DATA_NOT_INSTRUCTIONS}: \
             {guide_under_the_warning:?}"
        );
    }

    let get_ticket = delivered_description(tools, "get_ticket");
    assert!(
        sentences(&get_ticket)
            .iter()
            .any(|sentence| sentence.contains(DATA_NOT_INSTRUCTIONS) && sentence.contains("result")),
        "get_ticket must name the results among what is {DATA_NOT_INSTRUCTIONS}: {get_ticket:?}"
    );

    let list_tickets = delivered_description(tools, "list_tickets");
    assert!(
        sentences(&list_tickets).iter().any(|sentence| {
            sentence.contains("guide") && sentence.contains("user") && sentence.contains("rule")
        }),
        "list_tickets must present the guide as the rules the user set for the board: \
         {list_tickets:?}"
    );
}

#[test]
fn list_tickets_description_says_to_call_it_first_to_read_the_boards_rules() {
    let response = fetch_tools_list();
    let tools = tools_array(&response);
    let description = delivered_description(tools, "list_tickets");

    let missing = missing_groups(&description, &[&["first"], &["rule", "guide"]]);
    assert!(
        missing.is_empty(),
        "list_tickets must say to call it first to read the rules; missing {missing:?}: \
         {description:?}"
    );
}
