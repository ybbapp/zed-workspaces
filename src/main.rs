use anyhow::{anyhow, bail, Context, Result};
use pico_args::Arguments;
use serde_json::json;
use std::{
    env,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

const HELP: &str = r#"zed-workspaces 0.1.0

A small CLI to create and open .zed-workspace/.code-workspace files in Zed.

USAGE:
  zed-workspaces create [--name NAME] [DIRS...]
  zed-workspaces open [--reuse] [FILE]
  zed-workspaces install-tasks
  zed-workspaces update-from-settings FILE
  zed-workspaces --help

If create gets no DIRS, it prompts from directories in the current directory.
If open gets no FILE, it behaves like `open .`.
"#;

const ZED_WORKSPACE_SCHEMA: &str =
    "https://raw.githubusercontent.com/fu5ha/zed-workspaces/main/resources/zed-workspace-schema.json";

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
        "install-tasks" => install_tasks(),
        "update-from-settings" => update_from_settings(args),
        other => bail!("unknown command {other:?}\n\n{HELP}"),
    }
}

fn update_from_settings(mut args: Arguments) -> Result<()> {
    let file = args
        .opt_free_from_str::<String>()?
        .ok_or_else(|| anyhow!("update-from-settings requires $ZED_FILE"))?;
    let file = env::current_dir()?.join(file);
    let file = file.canonicalize().with_context(|| format!("failed to resolve {}", file.display()))?;

    let zed_dir = file
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", file.display()))?;
    if zed_dir.file_name().and_then(|name| name.to_str()) != Some(".zed") {
        bail!("{} is not inside a .zed directory", file.display());
    }

    let generated_dir = zed_dir
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", zed_dir.display()))?;
    if generated_dir.file_name().and_then(|name| name.to_str()) != Some(".zed-workspace") {
        bail!("{} is not inside .zed-workspace/.zed", file.display());
    }

    let workspace_dir = generated_dir
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", generated_dir.display()))?;
    let workspace_file = find_single_zed_workspace_in_dir(workspace_dir)?;

    let text = fs::read_to_string(&workspace_file)
        .with_context(|| format!("failed to read {}", workspace_file.display()))?;
    let mut workspace: serde_json::Value = json5::from_str(&text)
        .with_context(|| format!("invalid JSON/JSON5 in {}", workspace_file.display()))?;

    let old_object = workspace
        .as_object()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", workspace_file.display()))?;

    let mut object = serde_json::Map::new();
    object.insert("$schema".to_string(), json!(ZED_WORKSPACE_SCHEMA));
    for (key, value) in old_object {
        if key != "$schema" {
            object.insert(key.clone(), value.clone());
        }
    }

    let settings_file = zed_dir.join("settings.json");
    if settings_file.exists() {
        let settings_text = fs::read_to_string(&settings_file)
            .with_context(|| format!("failed to read {}", settings_file.display()))?;
        let settings: serde_json::Value = json5::from_str(&settings_text)
            .with_context(|| format!("invalid JSON/JSON5 in {}", settings_file.display()))?;
        object.insert("settings".to_string(), settings);
    }

    let debug_file = zed_dir.join("debug.json");
    if debug_file.exists() {
        let debug_text = fs::read_to_string(&debug_file)
            .with_context(|| format!("failed to read {}", debug_file.display()))?;
        let debug: serde_json::Value = json5::from_str(&debug_text)
            .with_context(|| format!("invalid JSON/JSON5 in {}", debug_file.display()))?;
        if !debug.is_array() {
            bail!("{} must contain a JSON array", debug_file.display());
        }
        object.insert("debug".to_string(), debug);
    }

    workspace = serde_json::Value::Object(object);

    fs::write(
        &workspace_file,
        format!("{}\n", serde_json::to_string_pretty(&workspace)?),
    )
    .with_context(|| format!("failed to write {}", workspace_file.display()))?;

    println!("Updated {}", workspace_file.display());
    Ok(())
}

fn find_single_zed_workspace_in_dir(dir: &Path) -> Result<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_file() && is_zed_workspace_file(&path) {
            files.push(path);
        }
    }

    match files.as_slice() {
        [file] => Ok(file.clone()),
        [] => bail!("no .zed-workspace file found in {}", dir.display()),
        _ => bail!("multiple .zed-workspace files found in {}; cannot choose non-interactively", dir.display()),
    }
}

fn install_tasks() -> Result<()> {
    let settings_file = find_zed_settings_file()?;
    let zed_config_dir = settings_file
        .parent()
        .ok_or_else(|| anyhow!("invalid Zed settings path: {}", settings_file.display()))?;
    let tasks_file = zed_config_dir.join("tasks.json");

    let wanted: serde_json::Value = serde_json::from_str(include_str!("../resources/tasks.json"))
        .context("bundled resources/tasks.json is invalid")?;
    let wanted_tasks = wanted
        .as_array()
        .ok_or_else(|| anyhow!("bundled resources/tasks.json must contain an array"))?;

    let mut current = if tasks_file.exists() {
        let text = fs::read_to_string(&tasks_file)
            .with_context(|| format!("failed to read {}", tasks_file.display()))?;
        json5::from_str::<serde_json::Value>(&text)
            .with_context(|| format!("invalid JSON/JSON5 in {}", tasks_file.display()))?
    } else {
        json!([])
    };

    let current_tasks = current
        .as_array_mut()
        .ok_or_else(|| anyhow!("{} must contain a JSON array", tasks_file.display()))?;

    for wanted_task in wanted_tasks {
        let label = wanted_task
            .get("label")
            .and_then(|label| label.as_str())
            .ok_or_else(|| anyhow!("task in bundled resources/tasks.json is missing a label"))?;

        if let Some(existing) = current_tasks.iter_mut().find(|task| {
            task.get("label")
                .and_then(|label| label.as_str())
                .is_some_and(|existing_label| existing_label == label)
        }) {
            *existing = wanted_task.clone();
        } else {
            current_tasks.push(wanted_task.clone());
        }
    }

    fs::create_dir_all(zed_config_dir)
        .with_context(|| format!("failed to create {}", zed_config_dir.display()))?;
    fs::write(&tasks_file, format!("{}\n", serde_json::to_string_pretty(&current)?))
        .with_context(|| format!("failed to write {}", tasks_file.display()))?;

    println!("Installed zed-workspaces tasks to {}", tasks_file.display());
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
            candidates.push(PathBuf::from(xdg_config_home).join("zed").join("settings.json"));
        }
        if let Some(home) = env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".config").join("zed").join("settings.json"));
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

    let mut dirs: Vec<String> = args.finish().into_iter().map(display_arg).collect();
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

    let mut content = serde_json::Map::new();
    content.insert("$schema".to_string(), json!(ZED_WORKSPACE_SCHEMA));
    content.insert(
        "folders".to_string(),
        dirs.into_iter().map(|dir| json!({ "path": dir })).collect(),
    );
    content.insert("settings".to_string(), json!({}));
    fs::write(&workspace_file, serde_json::to_string_pretty(&content)?)
        .with_context(|| format!("failed to write {}", workspace_file.display()))?;
    println!("Workspace successfully created at {}", workspace_file.display());
    Ok(())
}

fn open_workspace(mut args: Arguments) -> Result<()> {
    let root = env::current_dir()?;
    let reuse = args.contains("--reuse");
    let file = args.opt_free_from_str::<String>()?.map(PathBuf::from);
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

    open_zed_from_workspace(&file, reuse)
}

fn select_workspace_file_in_dir(root: &Path, dir: &Path) -> Result<PathBuf> {
    let files = find_workspaces_in_dir(dir)?;
    match files.as_slice() {
        [] => bail!("no .code-workspace files found directly in {}", dir.display()),
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
    Ok(if zed_workspaces.is_empty() { code_workspaces } else { zed_workspaces })
}

fn open_zed_from_workspace(file: &Path, reuse: bool) -> Result<()> {
    let text = fs::read_to_string(file).with_context(|| format!("workspace file not found at {}", file.display()))?;
    let value: serde_json::Value = json5::from_str(&text)
        .with_context(|| format!("invalid JSON/JSON5 in {}", file.display()))?;
    let folders = value["folders"].as_array().ok_or_else(|| anyhow!("invalid workspace: missing folders array"))?;
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    let mut paths: Vec<PathBuf> = folders
        .iter()
        .filter_map(|f| f.get("path").or_else(|| f.get("uri")).and_then(|v| v.as_str()))
        .map(|p| base.join(p))
        .collect();

    if is_zed_workspace_file(file) {
        let settings_dir = base.join(".zed-workspace");
        let zed_dir = settings_dir.join(".zed");
        fs::create_dir_all(&zed_dir)
            .with_context(|| format!("failed to create {}", zed_dir.display()))?;
        let settings = value.get("settings").cloned().unwrap_or_else(|| json!({}));
        fs::write(
            zed_dir.join("settings.json"),
            format!("{}\n", serde_json::to_string_pretty(&settings)?),
        )
        .with_context(|| format!("failed to write {}", zed_dir.join("settings.json").display()))?;

        if let Some(debug) = value.get("debug") {
            if !debug.is_array() {
                bail!("invalid .zed-workspace: debug must be an array");
            }
            fs::write(
                zed_dir.join("debug.json"),
                format!("{}\n", serde_json::to_string_pretty(debug)?),
            )
            .with_context(|| format!("failed to write {}", zed_dir.join("debug.json").display()))?;
        }

        paths.push(settings_dir);
    }

    if paths.is_empty() {
        bail!("no valid folders found in workspace");
    }

    println!("Opening {} folders in Zed...", paths.len());
    let mut cmd = Command::new("zed");
    cmd.args(&paths);
    cmd.arg(if reuse { "--reuse" } else { "--new" });
    cmd.spawn().context("failed to start `zed`")?;
    Ok(())
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
    if choices.is_empty() { bail!("no choices available"); }
    for (i, choice) in choices.iter().enumerate() { println!("{}: {}", i + 1, choice); }
    let n: usize = prompt(&format!("{message} [1-{}]: ", choices.len()))?.parse().context("invalid selection")?;
    choices.get(n - 1).cloned().ok_or_else(|| anyhow!("selection out of range"))
}

fn prompt_many(message: &str, choices: &[String]) -> Result<Vec<String>> {
    if choices.is_empty() { bail!("no directories found in the current root"); }
    for (i, choice) in choices.iter().enumerate() { println!("{}: {}", i + 1, choice); }
    let line = prompt(&format!("{message} (comma-separated numbers): "))?;
    line.split(',').map(|part| {
        let n: usize = part.trim().parse().context("invalid selection")?;
        choices.get(n - 1).cloned().ok_or_else(|| anyhow!("selection out of range"))
    }).collect()
}

fn display_arg(arg: std::ffi::OsString) -> String {
    arg.to_string_lossy().into_owned()
}
