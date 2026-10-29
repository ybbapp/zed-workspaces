use anyhow::{Context, Result, anyhow, bail};
use jsonc_parser::cst::{CstArray, CstInputValue, CstNode, CstRootNode};
use jsonc_parser::{JsonValue, parse_to_value};
use pico_args::Arguments;
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

const HELP: &str = r#"zed-workspaces

A small CLI to create and open .zed-workspace/.code-workspace files in Zed.

USAGE:
  zed-workspaces create [--name NAME] [DIRS...]
    Create NAME.zed-workspace with DIRS...
    If no DIRS provided, prompts from directories in the current directory.

  zed-workspaces open [--reuse] [FILE_OR_DIR]
    If no FILE_OR_DIR, it behaves like `open .`

  zed-workspaces install
    Install tasks and associate .zed-workspace files in your user-level zed settings.

  zed-workspaces --help
    Print this help text

"#;

const ZED_WORKSPACE_SCHEMA: &str = "https://raw.githubusercontent.com/fu5ha/zed-workspaces/main/resources/zed-workspace-schema.json";

fn main() -> Result<()> {
    let mut args = Arguments::from_env();
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return Ok(());
    }

    let command = args.subcommand()?.unwrap_or_else(|| "open".to_string());
    match command.as_str() {
        "create" => create_workspace(args),
        "open" => open_workspace(args),
        "install" => install(args),
        other => bail!("unknown command {other:?}\n\n{HELP}"),
    }
}

fn install(args: Arguments) -> Result<()> {
    ensure_no_args(args, "install")?;

    let settings_file = find_zed_settings_file()?;
    let zed_config_dir = settings_file
        .parent()
        .ok_or_else(|| anyhow!("invalid Zed settings path: {}", settings_file.display()))?;
    let tasks_file = zed_config_dir.join("tasks.json");

    let wanted = parse_jsonc(include_str!("../resources/tasks.json"))
        .context("bundled resources/tasks.json is invalid")?;
    let JsonValue::Array(wanted_tasks) = &wanted else {
        bail!("bundled resources/tasks.json must contain an array");
    };

    let current_text = if tasks_file.exists() {
        let text = fs::read_to_string(&tasks_file)
            .with_context(|| format!("failed to read {}", tasks_file.display()))?;
        if text.trim().is_empty() {
            "[]".to_string()
        } else {
            text
        }
    } else {
        "[]".to_string()
    };

    let root = CstRootNode::parse(&current_text, &Default::default())
        .with_context(|| format!("invalid JSONC in {}", tasks_file.display()))?;
    let current_tasks = root
        .value()
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow!("{} must contain a JSON array", tasks_file.display()))?;

    for wanted_task in wanted_tasks.iter() {
        let label = wanted_task
            .as_object_value()
            .and_then(|task| task.get_string("label"))
            .map(|label| label.as_ref())
            .ok_or_else(|| anyhow!("task in bundled resources/tasks.json is missing a label"))?;

        if let Some(existing) = current_tasks
            .elements()
            .into_iter()
            .find(|task| task_label(task).as_deref() == Some(label))
        {
            let index = existing
                .element_index()
                .unwrap_or_else(|| current_tasks.elements().len());
            existing.remove();
            current_tasks.insert(index, json_value_to_cst(wanted_task.clone()));
        } else {
            current_tasks.append(json_value_to_cst(wanted_task.clone()));
        }
    }

    fs::create_dir_all(zed_config_dir)
        .with_context(|| format!("failed to create {}", zed_config_dir.display()))?;
    backup_file_if_exists(&tasks_file)?;
    fs::write(&tasks_file, root.to_string())
        .with_context(|| format!("failed to write {}", tasks_file.display()))?;

    install_file_type_associations(&settings_file)?;

    println!("Installed zed-workspaces tasks to {}", tasks_file.display());
    println!(
        "Installed workspace file type associations to {}",
        settings_file.display()
    );
    Ok(())
}

fn install_file_type_associations(settings_file: &Path) -> Result<()> {
    let text = if settings_file.exists() {
        let text = fs::read_to_string(settings_file)
            .with_context(|| format!("failed to read {}", settings_file.display()))?;
        if text.trim().is_empty() {
            "{}".to_string()
        } else {
            text
        }
    } else {
        "{}".to_string()
    };

    let root = CstRootNode::parse(&text, &Default::default())
        .with_context(|| format!("invalid JSONC in {}", settings_file.display()))?;
    let settings = root.object_value_or_set();
    let file_types = settings.object_value_or_set("file_types");
    let jsonc = file_types.array_value_or_set("JSONC");

    append_string_if_missing(&jsonc, "*.code-workspace");
    append_string_if_missing(&jsonc, "*.zed-workspace");

    if let Some(parent) = settings_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    backup_file_if_exists(settings_file)?;
    fs::write(settings_file, root.to_string())
        .with_context(|| format!("failed to write {}", settings_file.display()))?;
    Ok(())
}

fn backup_file_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let backup = backup_path_next_to(path)?;
    backup_file_to(path, &backup)
}

fn backup_path_next_to(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid file name: {}", path.display()))?;
    Ok(path.with_file_name(format!("{file_name}.backup.{}", file_hash(path)?)))
}

fn file_hash(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let hash = tenthash::hash(&bytes);
    Ok(hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_string())
}

fn backup_file_to(path: &Path, backup: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    if !backup.exists() {
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(backup, bytes)
            .with_context(|| format!("failed to write backup {}", backup.display()))?;
        println!("Created backup {}", backup.display());
    }

    Ok(())
}

fn find_zed_settings_file() -> Result<PathBuf> {
    let candidates = zed_settings_candidates();
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| anyhow!("could not determine the Zed settings path"))
}

fn zed_settings_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if cfg!(windows) {
        if let Some(appdata) = env::var_os("APPDATA") {
            candidates.push(PathBuf::from(appdata).join("Zed").join("settings.json"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("Zed")
                    .join("settings.json"),
            );
        }
    } else {
        if let Some(xdg_config_home) = env::var_os("XDG_CONFIG_HOME") {
            candidates.push(
                PathBuf::from(xdg_config_home)
                    .join("zed")
                    .join("settings.json"),
            );
        }
        if let Some(home) = env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join(".config")
                    .join("zed")
                    .join("settings.json"),
            );
        }
    }

    candidates
}

fn create_workspace(mut args: Arguments) -> Result<()> {
    let root = env::current_dir()?;
    let name = match args.opt_value_from_str::<_, String>("--name")? {
        Some(name) => name,
        None => prompt("Enter a name for the new workspace (without extension): ")?,
    };
    let name = validate_workspace_name(&name)?;

    let mut dirs: Vec<String> = args
        .finish()
        .into_iter()
        .map(|arg| display_arg(&arg))
        .collect();
    if let Some(arg) = dirs.iter().find(|arg| arg.starts_with('-')) {
        bail!("unexpected argument for create: {arg}");
    }

    if dirs.is_empty() {
        dirs = prompt_many(
            "Select directories to include in the new workspace",
            &find_all_directories_on_root(&root)?,
        )?;
    }
    if dirs.is_empty() {
        bail!("no directories selected");
    }

    let workspace_file = root.join(format!("{name}.zed-workspace"));
    if workspace_file.exists() {
        bail!("a workspace named {name:?} already exists");
    }

    let content = CstInputValue::Object(vec![
        (
            "$schema".to_string(),
            CstInputValue::String(ZED_WORKSPACE_SCHEMA.to_string()),
        ),
        (
            "folders".to_string(),
            CstInputValue::Array(
                dirs.into_iter()
                    .map(|dir| {
                        CstInputValue::Object(vec![(
                            "path".to_string(),
                            CstInputValue::String(dir),
                        )])
                    })
                    .collect(),
            ),
        ),
    ]);
    fs::write(&workspace_file, cst_to_string(content)?)
        .with_context(|| format!("failed to write {}", workspace_file.display()))?;
    println!(
        "Workspace successfully created at {}",
        workspace_file.display()
    );
    Ok(())
}

fn open_workspace(mut args: Arguments) -> Result<()> {
    let root = env::current_dir()?;
    let reuse = args.contains("--reuse");
    let file = args.opt_free_from_str::<String>()?.map(PathBuf::from);
    ensure_no_args(args, "open")?;

    let file = match file {
        Some(path) if path.is_absolute() => path,
        Some(path) => root.join(path),
        None => root.clone(),
    };

    let file = if file.is_dir() {
        select_workspace_file_in_dir(&root, &file)?
    } else {
        file
    };

    open_workspace_file(&file, reuse)
}

fn select_workspace_file_in_dir(root: &Path, dir: &Path) -> Result<PathBuf> {
    let files = find_workspaces_in_dir(dir)?;
    match files.as_slice() {
        [] => bail!(
            "no .zed-workspace or .code-workspace files found directly in {}",
            dir.display()
        ),
        [file] => Ok(file.clone()),
        _ => {
            let choices: Vec<String> = files
                .iter()
                .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                .collect();
            let choice = prompt_one("Select a workspace file to open", &choices)?;
            Ok(root.join(choice))
        }
    }
}

fn find_workspaces_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut zed_workspaces = Vec::new();
    let mut code_workspaces = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if is_zed_workspace_file(&path) {
                zed_workspaces.push(path);
            } else if path.extension().is_some_and(|ext| ext == "code-workspace") {
                code_workspaces.push(path);
            }
        }
    }
    Ok(if zed_workspaces.is_empty() {
        code_workspaces
    } else {
        zed_workspaces
    })
}

fn open_workspace_file(file: &Path, reuse: bool) -> Result<()> {
    let paths = workspace_folder_paths(file)?;

    if paths.is_empty() {
        bail!("no valid folders found in workspace");
    }

    println!("Opening {} folders in Zed...", paths.len());
    let mut cmd = Command::new("zed");
    // Zed marks paths as trailing_var_arg, so flags after the first path are parsed as paths.
    cmd.arg(if reuse { "--reuse" } else { "--new" });
    cmd.args(&paths);
    cmd.spawn().context("failed to start `zed`")?;
    Ok(())
}

fn workspace_folder_paths(file: &Path) -> Result<Vec<PathBuf>> {
    let text = fs::read_to_string(file)
        .with_context(|| format!("workspace file not found at {}", file.display()))?;
    let value =
        parse_jsonc(&text).with_context(|| format!("invalid JSONC in {}", file.display()))?;
    let object = value
        .as_object_value()
        .ok_or_else(|| anyhow!("invalid workspace: root must be an object"))?;
    let folders = object
        .get_array("folders")
        .ok_or_else(|| anyhow!("invalid workspace: missing folders array"))?;
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    Ok(folders
        .iter()
        .filter_map(|folder| workspace_folder_path(folder.as_object_value()?, base))
        .collect())
}

fn workspace_folder_path(folder: &jsonc_parser::JsonObject<'_>, base: &Path) -> Option<PathBuf> {
    if let Some(path) = folder.get_string("path") {
        return Some(base.join(path.as_ref()));
    }

    let uri = folder.get_string("uri")?;
    let uri = uri.as_ref();
    if Path::new(uri).is_relative() {
        Some(base.join(uri))
    } else {
        eprintln!("Skipping non-relative workspace uri: {uri}");
        None
    }
}

fn is_zed_workspace_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "zed-workspace")
}

fn find_all_directories_on_root(root: &Path) -> Result<Vec<String>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && name != "node_modules" && !name.starts_with('.') {
            dirs.push(name);
        }
    }
    Ok(dirs)
}

fn validate_workspace_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains("..")
    {
        bail!("invalid workspace name");
    }
    Ok(trimmed.to_string())
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn prompt_one(message: &str, choices: &[String]) -> Result<String> {
    if choices.is_empty() {
        bail!("no choices available");
    }
    for (i, choice) in choices.iter().enumerate() {
        println!("{}: {}", i + 1, choice);
    }
    let n: usize = prompt(&format!("{message} [1-{}]: ", choices.len()))?
        .parse()
        .context("invalid selection")?;
    choices
        .get(n - 1)
        .cloned()
        .ok_or_else(|| anyhow!("selection out of range"))
}

fn prompt_many(message: &str, choices: &[String]) -> Result<Vec<String>> {
    if choices.is_empty() {
        bail!("no directories found in the current root");
    }
    for (i, choice) in choices.iter().enumerate() {
        println!("{}: {}", i + 1, choice);
    }
    let line = prompt(&format!("{message} (comma-separated numbers): "))?;
    line.split(',')
        .map(|part| {
            let n: usize = part.trim().parse().context("invalid selection")?;
            choices
                .get(n - 1)
                .cloned()
                .ok_or_else(|| anyhow!("selection out of range"))
        })
        .collect()
}

fn display_arg(arg: &std::ffi::OsString) -> String {
    arg.to_string_lossy().into_owned()
}

fn ensure_no_args(args: Arguments, command: &str) -> Result<()> {
    let args = args.finish();
    if let Some(arg) = args.first() {
        bail!("unexpected argument for {command}: {}", display_arg(arg));
    }
    Ok(())
}

fn parse_jsonc(text: &str) -> Result<JsonValue<'_>> {
    parse_to_value(text, &Default::default())?.ok_or_else(|| anyhow!("empty JSONC document"))
}

fn task_label(task: &CstNode) -> Option<String> {
    task.as_object()?
        .get("label")?
        .value()?
        .as_string_lit()?
        .decoded_value()
        .ok()
}

fn append_string_if_missing(array: &CstArray, value: &str) {
    let exists = array.elements().into_iter().any(|element| {
        element
            .as_string_lit()
            .and_then(|string| string.decoded_value().ok())
            .is_some_and(|existing| existing == value)
    });

    if !exists {
        array.append(CstInputValue::String(value.to_string()));
    }
}

fn json_value_to_cst(value: JsonValue<'_>) -> CstInputValue {
    match value {
        JsonValue::Null => CstInputValue::Null,
        JsonValue::Boolean(value) => CstInputValue::Bool(value),
        JsonValue::Number(value) => CstInputValue::Number(value.to_string()),
        JsonValue::String(value) => CstInputValue::String(value.into_owned()),
        JsonValue::Array(values) => {
            CstInputValue::Array(values.into_iter().map(json_value_to_cst).collect())
        }
        JsonValue::Object(values) => CstInputValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, json_value_to_cst(value)))
                .collect(),
        ),
    }
}

fn cst_to_string(value: CstInputValue) -> Result<String> {
    let root = CstRootNode::parse("null", &Default::default())?;
    root.set_value(value);
    Ok(format!("{}\n", root))
}

trait JsonValueExt<'a> {
    fn as_object_value(&'a self) -> Option<&'a jsonc_parser::JsonObject<'a>>;
}

impl<'a> JsonValueExt<'a> for JsonValue<'a> {
    fn as_object_value(&'a self) -> Option<&'a jsonc_parser::JsonObject<'a>> {
        match self {
            JsonValue::Object(object) => Some(object),
            _ => None,
        }
    }
}
