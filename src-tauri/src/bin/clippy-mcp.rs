use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    env, fs,
    io::Write,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const APP_ID: &str = "app.clippy.desktop";
const DATABASE_NAME: &str = "clippy.db";
const POLICY_NAME: &str = "mcp-policy.json";
const WAKE_SOCKET_NAME: &str = "clippy-mcp.sock";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_ITEM_CHARS: usize = 100_000;
const MAX_LIST_CHARS: usize = 80;
const MAX_QUERY_CHARS: usize = 500;
const HARD_MAX_RESULTS: u16 = 200;
const SKILL_MD: &str = include_str!("../../../.agents/skills/clippy-companion/SKILL.md");
const SKILL_OPENAI_YAML: &str =
    include_str!("../../../.agents/skills/clippy-companion/agents/openai.yaml");

fn main() {
    if let Err(error) = run() {
        eprintln!("clippy-mcp: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let paths = Paths::discover()?;
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("serve") {
        "serve" if args.len() == 1 || args.is_empty() => serve(paths),
        "show-policy" if args.len() == 1 => {
            print_json(&Policy::load(&paths.policy)?)?;
            Ok(())
        }
        "configure" => configure(&paths, &args[1..]),
        "doctor" if args.len() == 1 => doctor(&paths),
        "install-codex" if args.len() == 1 => install_codex(&paths),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err("unknown arguments; run `clippy-mcp help`".into()),
    }
}

fn print_help() {
    println!(
        "Clippy MCP (local stdio companion)\n\n\
         Usage:\n\
           clippy-mcp serve\n\
           clippy-mcp show-policy\n\
           clippy-mcp configure [options]\n\
           clippy-mcp doctor\n\
           clippy-mcp install-codex\n\n\
         Configure options:\n\
           --write-mode read-only|todos-only|manage-lists\n\
           --read-enabled true|false\n\
           --default-list NAME|none\n\
           --allow-inbox true|false\n\
           --allow-list-ids all|1,2,3\n\
           --include-completed true|false\n\
           --include-attachment-metadata true|false\n\
           --max-results 1..200"
    );
}

fn serve(paths: Paths) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let service = ClippyServer::new(paths)
            .serve(stdio())
            .await
            .map_err(|error| error.to_string())?;
        service
            .waiting()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[derive(Clone)]
struct Paths {
    database: PathBuf,
    policy: PathBuf,
    wake_socket: PathBuf,
    codex_home: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self, String> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| "HOME is unavailable".to_string())?;
        let data_dir = env::var_os("CLIPPY_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home.join("Library")
                    .join("Application Support")
                    .join(APP_ID)
            });
        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Ok(Self {
            database: data_dir.join(DATABASE_NAME),
            policy: data_dir.join(POLICY_NAME),
            wake_socket: data_dir.join(WAKE_SOCKET_NAME),
            codex_home,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WriteMode {
    ReadOnly,
    TodosOnly,
    ManageLists,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct Policy {
    read_enabled: bool,
    write_mode: WriteMode,
    default_list: Option<String>,
    allow_inbox: bool,
    allowed_list_ids: Vec<i64>,
    include_completed: bool,
    include_attachment_metadata: bool,
    max_results: u16,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            read_enabled: true,
            write_mode: WriteMode::ReadOnly,
            default_list: None,
            allow_inbox: true,
            allowed_list_ids: Vec::new(),
            include_completed: false,
            include_attachment_metadata: false,
            max_results: 50,
        }
    }
}

impl Policy {
    fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        if metadata.len() > MAX_POLICY_BYTES {
            return Err("MCP policy is too large; refusing access".into());
        }
        let policy: Self = serde_json::from_slice(
            &fs::read(path).map_err(|error| format!("could not read MCP policy: {error}"))?,
        )
        .map_err(|error| format!("invalid MCP policy; refusing access: {error}"))?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_results == 0 || self.max_results > HARD_MAX_RESULTS {
            return Err(format!("max_results must be 1..={HARD_MAX_RESULTS}"));
        }
        if self.allowed_list_ids.len() > 128
            || self.allowed_list_ids.iter().any(|id| *id <= 0)
            || self
                .allowed_list_ids
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                != self.allowed_list_ids.len()
        {
            return Err("allowed_list_ids must contain up to 128 unique positive IDs".into());
        }
        if let Some(name) = &self.default_list {
            validate_list_name(name)?;
        }
        Ok(())
    }

    fn allows_list(&self, id: Option<i64>) -> bool {
        match id {
            None => self.allow_inbox,
            Some(id) => self.allowed_list_ids.is_empty() || self.allowed_list_ids.contains(&id),
        }
    }

    fn result_limit(&self, requested: Option<u16>) -> usize {
        requested
            .unwrap_or(self.max_results)
            .max(1)
            .min(self.max_results)
            .min(HARD_MAX_RESULTS) as usize
    }
}

fn configure(paths: &Paths, args: &[String]) -> Result<(), String> {
    let mut policy = Policy::load(&paths.policy)?;
    if args.is_empty() {
        print_json(&policy)?;
        println!("Run `clippy-mcp help` to see configurable fields.");
        return Ok(());
    }
    if args.len() % 2 != 0 {
        return Err("every configure option requires a value".into());
    }
    for pair in args.chunks_exact(2) {
        let option = pair[0].as_str();
        let value = pair[1].as_str();
        match option {
            "--write-mode" => {
                policy.write_mode = match value {
                    "read-only" | "read_only" => WriteMode::ReadOnly,
                    "todos-only" | "todos_only" => WriteMode::TodosOnly,
                    "manage-lists" | "manage_lists" => WriteMode::ManageLists,
                    _ => {
                        return Err(
                            "write mode must be read-only, todos-only, or manage-lists".into()
                        )
                    }
                }
            }
            "--read-enabled" => policy.read_enabled = parse_bool(value)?,
            "--default-list" => {
                policy.default_list = if value.eq_ignore_ascii_case("none") {
                    None
                } else {
                    validate_list_name(value)?;
                    Some(value.trim().to_string())
                }
            }
            "--allow-inbox" => policy.allow_inbox = parse_bool(value)?,
            "--allow-list-ids" => {
                policy.allowed_list_ids = if value.eq_ignore_ascii_case("all") {
                    Vec::new()
                } else {
                    value
                        .split(',')
                        .map(|part| {
                            part.trim().parse::<i64>().map_err(|_| {
                                "allow-list-ids must be `all` or comma-separated IDs".to_string()
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
            }
            "--include-completed" => policy.include_completed = parse_bool(value)?,
            "--include-attachment-metadata" => {
                policy.include_attachment_metadata = parse_bool(value)?
            }
            "--max-results" => {
                policy.max_results = value
                    .parse::<u16>()
                    .map_err(|_| "max-results must be an integer".to_string())?
            }
            _ => return Err(format!("unknown configure option: {option}")),
        }
    }
    policy.validate()?;
    save_policy(&paths.policy, &policy)?;
    println!("Clippy MCP policy updated:");
    print_json(&policy)
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err("boolean values must be true or false".into()),
    }
}

fn save_policy(path: &Path, policy: &Policy) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "policy path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(policy).map_err(|error| error.to_string())?;
    atomic_write(path, &bytes, 0o600)
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "target path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .map_err(|error| error.to_string())?;
    temporary.flush().map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

fn install_codex(paths: &Paths) -> Result<(), String> {
    if !paths.policy.exists() {
        save_policy(&paths.policy, &Policy::default())?;
    }
    let skill_dir = paths.codex_home.join("skills").join("clippy-companion");
    atomic_write(&skill_dir.join("SKILL.md"), SKILL_MD.as_bytes(), 0o644)?;
    atomic_write(
        &skill_dir.join("agents").join("openai.yaml"),
        SKILL_OPENAI_YAML.as_bytes(),
        0o644,
    )?;

    let codex = codex_executable().ok_or_else(|| {
        "Codex is not installed. Install the ChatGPT desktop app or Codex CLI first.".to_string()
    })?;
    let already_configured = Command::new(&codex)
        .args(["mcp", "get", "clippy"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !already_configured {
        let executable = env::current_exe()
            .map_err(|error| error.to_string())?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let status = Command::new(&codex)
            .args(["mcp", "add", "clippy", "--"])
            .arg(executable)
            .arg("serve")
            .status()
            .map_err(|error| format!("could not run `codex mcp add`: {error}"))?;
        if !status.success() {
            return Err("Codex rejected the Clippy MCP configuration".into());
        }
    }

    println!("Clippy Companion is installed with a read-only default policy.");
    println!("Start a new Codex task (or restart the local client), then ask to configure Clippy.");
    Ok(())
}

fn doctor(paths: &Paths) -> Result<(), String> {
    let policy = Policy::load(&paths.policy)?;
    let connection = open_database(&paths.database, false)?;
    validate_schema(&connection)?;
    let configured = codex_executable()
        .and_then(|codex| {
            Command::new(codex)
                .args(["mcp", "get", "clippy"])
                .output()
                .ok()
        })
        .map(|output| output.status.success())
        .unwrap_or(false);
    let skill = paths
        .codex_home
        .join("skills/clippy-companion/SKILL.md")
        .is_file();
    println!("database: ok ({})", paths.database.display());
    println!("policy: ok ({:?})", policy.write_mode);
    println!(
        "codex_mcp: {}",
        if configured {
            "configured"
        } else {
            "not configured"
        }
    );
    println!(
        "companion_skill: {}",
        if skill { "installed" } else { "not installed" }
    );
    Ok(())
}

fn codex_executable() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("CLIPPY_CODEX_CLI").map(PathBuf::from) {
        if explicit.is_absolute() && explicit.is_file() {
            return Some(explicit);
        }
    }
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let candidate = directory.join("codex");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let mut candidates = vec![
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/opt/homebrew/bin/codex"),
    ];
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".local/bin/codex"));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListItemsRequest {
    /// Existing list ID. Use either list_id or list_name, not both.
    list_id: Option<i64>,
    /// Existing list name, or Inbox for unfiled items.
    list_name: Option<String>,
    /// Optional case-insensitive phrase contained in the item.
    query: Option<String>,
    /// Request completed items. The local policy must also permit them.
    include_completed: Option<bool>,
    /// Maximum results, capped by local policy and 200.
    limit: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchRequest {
    /// Case-insensitive phrase to find in saved item text.
    query: String,
    /// Optionally restrict the search to an existing list ID.
    list_id: Option<i64>,
    /// Optionally restrict the search to an existing list name or Inbox.
    list_name: Option<String>,
    /// Request completed items. The local policy must also permit them.
    include_completed: Option<bool>,
    /// Maximum results, capped by local policy and 200.
    limit: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AddTodoRequest {
    /// Exact todo text to save.
    content: String,
    /// Existing destination list ID.
    list_id: Option<i64>,
    /// Existing destination list name or Inbox.
    list_name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateTodoRequest {
    /// Existing Clippy item ID.
    item_id: i64,
    /// Replacement todo text.
    content: String,
    /// Optional existing destination list ID.
    list_id: Option<i64>,
    /// Optional existing destination list name or Inbox.
    list_name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetDoneRequest {
    /// Existing Clippy item ID.
    item_id: i64,
    /// True to complete the todo, false to reopen it.
    done: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateListRequest {
    /// New list name. Creation requires manage_lists policy.
    name: String,
}

#[derive(Clone)]
struct ClippyServer {
    paths: Paths,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ClippyServer {
    fn new(paths: Paths) -> Self {
        Self {
            paths,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Show the active local Clippy agent policy. Call this before the first Clippy operation in a task."
    )]
    fn clippy_get_policy(&self) -> Result<CallToolResult, McpError> {
        Ok(tool_result(Policy::load(&self.paths.policy).and_then(
            |policy| serde_json::to_value(policy).map_err(|error| error.to_string()),
        )))
    }

    #[tool(
        description = "List the Clippy lists the local policy allows, with visible open and completed item counts."
    )]
    fn clippy_list_lists(&self) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.list_lists()))
    }

    #[tool(
        description = "Read bounded items from one Clippy list. List names and IDs must already exist; attachment contents and paths are never returned."
    )]
    fn clippy_read_list(
        &self,
        Parameters(request): Parameters<ListItemsRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.read_list(request)))
    }

    #[tool(
        description = "Search bounded saved Clippy item text, optionally within one list. Use only when the user asks to search their saved data."
    )]
    fn clippy_search(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.search(request)))
    }

    #[tool(
        description = "Add an explicit todo to an existing Clippy list. Requires todos_only or manage_lists policy."
    )]
    fn clippy_add_todo(
        &self,
        Parameters(request): Parameters<AddTodoRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.add_todo(request)))
    }

    #[tool(
        description = "Edit an existing Clippy todo and optionally move it to an existing list. Requires explicit user intent and write policy."
    )]
    fn clippy_update_todo(
        &self,
        Parameters(request): Parameters<UpdateTodoRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.update_todo(request)))
    }

    #[tool(
        description = "Complete or reopen an existing Clippy todo. Requires explicit user intent and write policy."
    )]
    fn clippy_set_todo_done(
        &self,
        Parameters(request): Parameters<SetDoneRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.set_done(request)))
    }

    #[tool(
        description = "Create a new Clippy list. Requires manage_lists policy; agents can never delete lists."
    )]
    fn clippy_create_list(
        &self,
        Parameters(request): Parameters<CreateListRequest>,
    ) -> Result<CallToolResult, McpError> {
        Ok(tool_result(self.create_list(request)))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ClippyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("clippy-mcp", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Clippy contains the user's personal local lists and todos. Call clippy_get_policy first. Read only when the user asks and keep queries narrow. Write only on explicit user request; never infer a todo or destination. Resolve list names with clippy_list_lists. Policy cannot be changed through MCP. Attachment contents and paths are unavailable. Deletion is not exposed."
            )
    }
}

impl ClippyServer {
    fn policy(&self) -> Result<Policy, String> {
        Policy::load(&self.paths.policy)
    }

    fn list_lists(&self) -> Result<Value, String> {
        let policy = self.policy()?;
        require_reads(&policy)?;
        let connection = open_database(&self.paths.database, false)?;
        validate_schema(&connection)?;
        let mut lists = Vec::new();
        if policy.allow_inbox {
            let (open, completed): (i64, i64) = connection
                .query_row(
                    "SELECT SUM(CASE WHEN done = 0 THEN 1 ELSE 0 END), SUM(CASE WHEN done = 1 THEN 1 ELSE 0 END) FROM items WHERE section_id IS NULL",
                    [],
                    |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0), row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
                )
                .map_err(|error| error.to_string())?;
            lists.push(json!({
                "id": null,
                "name": "Inbox",
                "open_count": open,
                "completed_count": if policy.include_completed { Value::from(completed) } else { Value::Null }
            }));
        }
        let mut statement = connection
            .prepare(
                "SELECT sections.id, sections.name,
                        SUM(CASE WHEN items.done = 0 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN items.done = 1 THEN 1 ELSE 0 END)
                 FROM sections LEFT JOIN items ON items.section_id = sections.id
                 GROUP BY sections.id, sections.name ORDER BY sections.id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (id, name, open, completed) = row.map_err(|error| error.to_string())?;
            if policy.allows_list(Some(id)) {
                lists.push(json!({
                    "id": id,
                    "name": name,
                    "open_count": open,
                    "completed_count": if policy.include_completed { Value::from(completed) } else { Value::Null }
                }));
            }
        }
        Ok(json!({ "lists": lists, "write_mode": policy.write_mode }))
    }

    fn read_list(&self, request: ListItemsRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        require_reads(&policy)?;
        validate_query(request.query.as_deref())?;
        let connection = open_database(&self.paths.database, false)?;
        validate_schema(&connection)?;
        let list = resolve_list(
            &connection,
            &policy,
            request.list_id,
            request.list_name.as_deref(),
            true,
        )?;
        let include_completed =
            policy.include_completed && request.include_completed.unwrap_or(false);
        let limit = policy.result_limit(request.limit);
        let query = request.query.unwrap_or_default();
        let mut statement = connection
            .prepare(
                "SELECT id, content, done, created_at, updated_at
                 FROM items
                 WHERE section_id IS ?1
                   AND (?2 = '' OR instr(lower(content), lower(?2)) > 0)
                   AND (?3 = 1 OR done = 0)
                 ORDER BY updated_at DESC, id DESC LIMIT ?4",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![list.id, query, include_completed as i64, limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            let (id, content, done, created_at, updated_at) =
                row.map_err(|error| error.to_string())?;
            let attachments = if policy.include_attachment_metadata {
                attachment_metadata(&connection, id)?
            } else {
                Vec::new()
            };
            items.push(json!({
                "id": id,
                "content": content,
                "done": done,
                "created_at": created_at,
                "updated_at": updated_at,
                "attachments": attachments
            }));
        }
        Ok(json!({ "list": list, "items": items, "limit": limit }))
    }

    fn search(&self, request: SearchRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        require_reads(&policy)?;
        let query = request.query.trim();
        validate_query(Some(query))?;
        if query.is_empty() {
            return Err("search query cannot be empty".into());
        }
        if request.list_id.is_some() && request.list_name.is_some() {
            return Err("use either list_id or list_name, not both".into());
        }
        let connection = open_database(&self.paths.database, false)?;
        validate_schema(&connection)?;
        let selected = if request.list_id.is_some() || request.list_name.is_some() {
            Some(resolve_list(
                &connection,
                &policy,
                request.list_id,
                request.list_name.as_deref(),
                false,
            )?)
        } else {
            None
        };
        let include_completed =
            policy.include_completed && request.include_completed.unwrap_or(false);
        let limit = policy.result_limit(request.limit);
        let fetch_limit = (limit * 20).max(limit).min(4_000);
        let mut statement = connection
            .prepare(
                "SELECT items.id, items.section_id, COALESCE(sections.name, 'Inbox'),
                        items.content, items.done, items.created_at, items.updated_at
                 FROM items LEFT JOIN sections ON sections.id = items.section_id
                 WHERE instr(lower(items.content), lower(?1)) > 0
                   AND (?2 = 1 OR items.done = 0)
                   AND (?3 = 0 OR items.section_id IS ?4)
                 ORDER BY items.updated_at DESC, items.id DESC LIMIT ?5",
            )
            .map_err(|error| error.to_string())?;
        let restrict = selected.is_some();
        let selected_id = selected.as_ref().and_then(|list| list.id);
        let rows = statement
            .query_map(
                params![
                    query,
                    include_completed as i64,
                    restrict as i64,
                    selected_id,
                    fetch_limit as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? != 0,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            let (id, section_id, list_name, content, done, created_at, updated_at) =
                row.map_err(|error| error.to_string())?;
            if !policy.allows_list(section_id) {
                continue;
            }
            items.push(json!({
                "id": id,
                "list": { "id": section_id, "name": list_name },
                "content": content,
                "done": done,
                "created_at": created_at,
                "updated_at": updated_at
            }));
            if items.len() == limit {
                break;
            }
        }
        Ok(json!({ "query": query, "items": items, "limit": limit }))
    }

    fn add_todo(&self, request: AddTodoRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        require_todo_writes(&policy)?;
        let content = validate_item_content(&request.content)?;
        let mut connection = open_database(&self.paths.database, true)?;
        validate_schema(&connection)?;
        let list = resolve_list(
            &connection,
            &policy,
            request.list_id,
            request.list_name.as_deref(),
            true,
        )?;
        let now = now_ms();
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO items(section_id, content, done, created_at, updated_at) VALUES(?1, ?2, 0, ?3, ?3)",
                params![list.id, content, now],
            )
            .map_err(|error| error.to_string())?;
        let item_id = transaction.last_insert_rowid();
        transaction.commit().map_err(|error| error.to_string())?;
        self.notify_app();
        Ok(json!({ "item": { "id": item_id, "content": content, "done": false, "list": list } }))
    }

    fn update_todo(&self, request: UpdateTodoRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        require_todo_writes(&policy)?;
        if request.item_id <= 0 {
            return Err("item_id must be positive".into());
        }
        let content = validate_item_content(&request.content)?;
        let mut connection = open_database(&self.paths.database, true)?;
        validate_schema(&connection)?;
        let current_section: Option<i64> = connection
            .query_row(
                "SELECT section_id FROM items WHERE id = ?1",
                params![request.item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "that Clippy item no longer exists".to_string())?;
        if !policy.allows_list(current_section) {
            return Err("local MCP policy does not permit that item's list".into());
        }
        let destination = if request.list_id.is_some() || request.list_name.is_some() {
            resolve_list(
                &connection,
                &policy,
                request.list_id,
                request.list_name.as_deref(),
                false,
            )?
        } else {
            list_by_id(&connection, current_section)?
        };
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE items SET content = ?2, section_id = ?3, updated_at = ?4 WHERE id = ?1",
                params![request.item_id, content, destination.id, now_ms()],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        self.notify_app();
        Ok(json!({ "item": { "id": request.item_id, "content": content, "list": destination } }))
    }

    fn set_done(&self, request: SetDoneRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        require_todo_writes(&policy)?;
        if request.item_id <= 0 {
            return Err("item_id must be positive".into());
        }
        let connection = open_database(&self.paths.database, true)?;
        validate_schema(&connection)?;
        let section_id: Option<i64> = connection
            .query_row(
                "SELECT section_id FROM items WHERE id = ?1",
                params![request.item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "that Clippy item no longer exists".to_string())?;
        if !policy.allows_list(section_id) {
            return Err("local MCP policy does not permit that item's list".into());
        }
        connection
            .execute(
                "UPDATE items SET done = ?2, updated_at = ?3 WHERE id = ?1",
                params![request.item_id, request.done as i64, now_ms()],
            )
            .map_err(|error| error.to_string())?;
        self.notify_app();
        Ok(json!({ "item": { "id": request.item_id, "done": request.done } }))
    }

    fn create_list(&self, request: CreateListRequest) -> Result<Value, String> {
        let policy = self.policy()?;
        if policy.write_mode != WriteMode::ManageLists {
            return Err("local MCP policy does not permit list creation".into());
        }
        if !policy.allowed_list_ids.is_empty() {
            return Err("list creation is disabled while policy restricts named list IDs".into());
        }
        let name = validate_list_name(&request.name)?;
        let connection = open_database(&self.paths.database, true)?;
        validate_schema(&connection)?;
        let existing: Option<i64> = connection
            .query_row(
                "SELECT id FROM sections WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if existing.is_some() {
            return Err("a Clippy list with that name already exists".into());
        }
        connection
            .execute(
                "INSERT INTO sections(name, created_at) VALUES(?1, ?2)",
                params![name, now_ms()],
            )
            .map_err(|error| error.to_string())?;
        let id = connection.last_insert_rowid();
        self.notify_app();
        Ok(json!({ "list": { "id": id, "name": name } }))
    }

    fn notify_app(&self) {
        if let Ok(socket) = UnixDatagram::unbound() {
            let _ = socket.send_to(b"refresh", &self.paths.wake_socket);
        }
    }
}

#[derive(Debug, Serialize)]
struct ListRef {
    id: Option<i64>,
    name: String,
}

fn resolve_list(
    connection: &Connection,
    policy: &Policy,
    list_id: Option<i64>,
    list_name: Option<&str>,
    use_default: bool,
) -> Result<ListRef, String> {
    if list_id.is_some() && list_name.is_some() {
        return Err("use either list_id or list_name, not both".into());
    }
    let list = if let Some(id) = list_id {
        if id <= 0 {
            return Err("list_id must be positive".into());
        }
        list_by_id(connection, Some(id))?
    } else if let Some(name) = list_name {
        list_by_name(connection, name)?
    } else if use_default {
        if let Some(name) = policy.default_list.as_deref() {
            list_by_name(connection, name)?
        } else {
            let active = connection
                .query_row(
                    "SELECT value FROM settings WHERE key = 'active_section'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .and_then(|value| value.parse::<i64>().ok());
            list_by_id(connection, active)?
        }
    } else {
        return Err("a list_id or list_name is required".into());
    };
    if !policy.allows_list(list.id) {
        return Err("local MCP policy does not permit that list".into());
    }
    Ok(list)
}

fn list_by_id(connection: &Connection, id: Option<i64>) -> Result<ListRef, String> {
    match id {
        None => Ok(ListRef {
            id: None,
            name: "Inbox".into(),
        }),
        Some(id) => connection
            .query_row(
                "SELECT name FROM sections WHERE id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .map(|name| ListRef { id: Some(id), name })
            .ok_or_else(|| "that Clippy list no longer exists".into()),
    }
}

fn list_by_name(connection: &Connection, name: &str) -> Result<ListRef, String> {
    let name = validate_list_name(name)?;
    if name.eq_ignore_ascii_case("Inbox") {
        return Ok(ListRef {
            id: None,
            name: "Inbox".into(),
        });
    }
    connection
        .query_row(
            "SELECT id, name FROM sections WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |row| {
                Ok(ListRef {
                    id: Some(row.get(0)?),
                    name: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "that Clippy list does not exist".into())
}

fn attachment_metadata(connection: &Connection, item_id: i64) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, name, media_type, size FROM attachments WHERE item_id = ?1 ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![item_id], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "name": row.get::<_, String>(1)?,
                "media_type": row.get::<_, String>(2)?,
                "size": row.get::<_, i64>(3)?
            }))
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn require_reads(policy: &Policy) -> Result<(), String> {
    if policy.read_enabled {
        Ok(())
    } else {
        Err("local MCP policy has disabled reads".into())
    }
}

fn require_todo_writes(policy: &Policy) -> Result<(), String> {
    if matches!(
        policy.write_mode,
        WriteMode::TodosOnly | WriteMode::ManageLists
    ) {
        Ok(())
    } else {
        Err("local MCP policy is read-only".into())
    }
}

fn validate_item_content(content: &str) -> Result<&str, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err("todo content cannot be empty".into());
    }
    if content.chars().count() > MAX_ITEM_CHARS {
        return Err(format!(
            "todo content cannot exceed {MAX_ITEM_CHARS} characters"
        ));
    }
    Ok(content)
}

fn validate_list_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_LIST_CHARS {
        return Err(format!(
            "list names must contain 1..={MAX_LIST_CHARS} characters"
        ));
    }
    if name.contains(['\n', '\r']) {
        return Err("list names must fit on one line".into());
    }
    Ok(name)
}

fn validate_query(query: Option<&str>) -> Result<(), String> {
    if query.is_some_and(|query| query.chars().count() > MAX_QUERY_CHARS) {
        return Err(format!("query cannot exceed {MAX_QUERY_CHARS} characters"));
    }
    Ok(())
}

fn open_database(path: &Path, writable: bool) -> Result<Connection, String> {
    if !path.is_file() {
        return Err(format!("Clippy database not found at {}", path.display()));
    }
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(path, flags).map_err(|error| error.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn validate_schema(connection: &Connection) -> Result<(), String> {
    for table in ["sections", "items", "settings", "attachments"] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![table],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !exists {
            return Err(format!("Clippy database is missing the {table} table"));
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn tool_result(result: Result<Value, String>) -> CallToolResult {
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(value);
            result
        }
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ClippyServer) {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let database = data_dir.join(DATABASE_NAME);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE sections(id INTEGER PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
                 CREATE TABLE items(id INTEGER PRIMARY KEY, section_id INTEGER REFERENCES sections(id), content TEXT NOT NULL, done INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE attachments(id INTEGER PRIMARY KEY, item_id INTEGER NOT NULL REFERENCES items(id), name TEXT NOT NULL, stored_path TEXT NOT NULL, media_type TEXT NOT NULL, size INTEGER NOT NULL, created_at INTEGER NOT NULL);
                 INSERT INTO sections(id,name,created_at) VALUES(1,'Work',1);",
            )
            .unwrap();
        drop(connection);
        let paths = Paths {
            database: database.clone(),
            policy: data_dir.join(POLICY_NAME),
            wake_socket: data_dir.join(WAKE_SOCKET_NAME),
            codex_home: root.path().join("codex"),
        };
        let server = ClippyServer::new(paths);
        (root, server)
    }

    #[test]
    fn default_policy_is_read_only_and_bounded() {
        let policy = Policy::default();
        assert_eq!(policy.write_mode, WriteMode::ReadOnly);
        assert_eq!(policy.result_limit(Some(500)), 50);
        assert!(!policy.include_attachment_metadata);
    }

    #[test]
    fn policy_rejects_duplicate_allowlist_ids() {
        let policy = Policy {
            allowed_list_ids: vec![1, 1],
            ..Policy::default()
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn todo_write_requires_policy_and_uses_existing_list() {
        let (_root, server) = fixture();
        let request = AddTodoRequest {
            content: "Ship the MCP companion".into(),
            list_id: Some(1),
            list_name: None,
        };
        assert!(server.add_todo(request).is_err());

        save_policy(
            &server.paths.policy,
            &Policy {
                write_mode: WriteMode::TodosOnly,
                ..Policy::default()
            },
        )
        .unwrap();
        let result = server
            .add_todo(AddTodoRequest {
                content: "Ship the MCP companion".into(),
                list_id: Some(1),
                list_name: None,
            })
            .unwrap();
        assert_eq!(result["item"]["list"]["name"], "Work");
        assert_eq!(result["item"]["content"], "Ship the MCP companion");
    }

    #[test]
    fn read_policy_hides_completed_and_attachment_paths() {
        let (_root, server) = fixture();
        let connection = Connection::open(&server.paths.database).unwrap();
        connection.execute("INSERT INTO items(id,section_id,content,done,created_at,updated_at) VALUES(1,1,'Open',0,1,1),(2,1,'Done',1,2,2)", []).unwrap();
        connection.execute("INSERT INTO attachments(id,item_id,name,stored_path,media_type,size,created_at) VALUES(1,1,'secret.txt','/private/secret.txt','text/plain',12,1)", []).unwrap();
        drop(connection);
        let result = server
            .read_list(ListItemsRequest {
                list_id: Some(1),
                list_name: None,
                query: None,
                include_completed: Some(true),
                limit: None,
            })
            .unwrap();
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["attachments"], json!([]));
        assert!(!result.to_string().contains("/private/secret.txt"));
    }
}
