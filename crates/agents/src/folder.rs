//! Where teams and agents live on disk: `<root>/<team>/team.json`, the team's `runs/`, and a
//! folder for each agent at the top of a session, laid out as the agent kit's.
//!
//! The app rewrites only the files it owns ([`OWNED`]); the agent's own files are made only when
//! missing, so its memory, notes and runs are never overwritten. Nothing is deleted: archiving
//! moves a folder into `<root>/archive/`.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{Value, json};

use crate::{
    plan::Session,
    prompts,
    team::{LabData, Team, home},
};

/// The files of an agent's folder that the app writes on every run; the rest are the agent's.
pub const OWNED: [&str; 8] = [
    "CLAUDE.md",
    "principles.md",
    "subject.md",
    ".claude/settings.json",
    ".claude/members.json",
    ".mcp.json",
    "bin/commit.sh",
    ".gitignore",
];

/// Its own files, as a new agent gets them.
const OWN: [(&str, &str); 8] = [
    (
        "memory/index.md",
        include_str!("../template/memory/index.md"),
    ),
    (
        "memory/beliefs.md",
        include_str!("../template/memory/beliefs.md"),
    ),
    (
        "memory/tried.md",
        include_str!("../template/memory/tried.md"),
    ),
    (
        "memory/questions.md",
        include_str!("../template/memory/questions.md"),
    ),
    ("memory/lab.md", include_str!("../template/memory/lab.md")),
    ("app/bugs.md", include_str!("../template/app/bugs.md")),
    ("app/ideas.md", include_str!("../template/app/ideas.md")),
    ("vetoed.md", include_str!("../template/vetoed.md")),
];

const COMMIT_SH: &str = include_str!("../template/commit.sh");

/// The flag the app leaves in an agent's `.claude/` when its budget is used, which its hook
/// then reads.
pub const WRAP_UP: &str = ".claude/wrap-up";

/// `~/persephone-agents`.
#[must_use]
pub fn default_root() -> PathBuf {
    home().join("persephone-agents")
}

#[must_use]
pub fn team_dir(root: &Path, team: &Team) -> PathBuf {
    root.join(&team.id)
}

#[must_use]
pub fn agent_dir(root: &Path, team: &Team, agent: &str) -> PathBuf {
    team_dir(root, team).join(agent)
}

/// Saves the design as `team.json`, written aside and moved so it's never half there.
///
/// # Errors
///
/// Says why it couldn't be written.
pub fn save_team(root: &Path, team: &Team) -> Result<(), String> {
    let dir = team_dir(root, team);
    fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(team).map_err(|e| e.to_string())?;
    write_aside(&dir.join("team.json"), text.as_bytes())
}

/// Every team under `root`, by name; a folder whose `team.json` doesn't read is passed over.
#[must_use]
pub fn load_teams(root: &Path) -> Vec<Team> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut teams: Vec<Team> = entries
        .flatten()
        .filter(|entry| entry.file_name() != "archive")
        .filter_map(|entry| fs::read_to_string(entry.path().join("team.json")).ok())
        .filter_map(|text| serde_json::from_str::<Team>(&text).ok())
        .collect();
    teams.sort_by_key(|t| t.name.to_lowercase());
    teams
}

/// Moves `path` into `<root>/archive/`, under its own name and the time, and says where.
///
/// # Errors
///
/// Says why it couldn't be moved.
pub fn archive(root: &Path, path: &Path, name: &str) -> Result<PathBuf, String> {
    let archive = root.join("archive");
    fs::create_dir_all(&archive).map_err(|e| e.to_string())?;
    let mut to = archive.join(format!("{name}-{}", stamp()));
    let mut n = 2;
    while to.exists() {
        to = archive.join(format!("{name}-{}-{n}", stamp()));
        n += 1;
    }
    fs::rename(path, &to).map_err(|e| format!("couldn't archive {}: {e}", path.display()))?;
    Ok(to)
}

/// What a run needs to know about this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// Where the teams are.
    pub root: PathBuf,
    /// Claude Code's program.
    pub claude: PathBuf,
    /// Lab's program, when one was found.
    pub lab: Option<PathBuf>,
    /// Folders no agent may read, such as Persephone's code.
    pub keep_out: Vec<PathBuf>,
}

/// Makes `session`'s folder, or brings the app's files in it up to date, and says where it is.
///
/// # Errors
///
/// Says what couldn't be written.
pub fn refresh(setup: &Setup, team: &Team, session: &Session) -> Result<PathBuf, String> {
    let dir = agent_dir(&setup.root, team, &session.root);
    let fresh = !dir.join(".git").exists();
    for folder in [
        "memory/journal",
        "memory/team",
        "app",
        "runs",
        "inbox",
        "outbox",
        "bin",
        ".claude",
        "lab-answers",
    ] {
        fs::create_dir_all(dir.join(folder))
            .map_err(|e| format!("couldn't make {}: {e}", dir.join(folder).display()))?;
    }
    for (path, text) in OWN {
        let path = dir.join(path);
        if !path.exists() {
            write_aside(&path, text.as_bytes())?;
        }
    }
    let tasks = dir.join("tasks.md");
    if !tasks.exists() {
        write_aside(&tasks, prompts::tasks_md(session.lab).as_bytes())?;
    }

    let owned = [
        ("CLAUDE.md", prompts::claude_md(team, session)),
        ("principles.md", ensure_newline(&team.principles)),
        ("subject.md", prompts::subject_md(team)),
        (
            ".claude/settings.json",
            pretty(&settings(setup, team, session, &dir)),
        ),
        (
            ".claude/members.json",
            pretty(&prompts::members_json(team, session)),
        ),
        (".mcp.json", pretty(&mcp(setup, team, session, &dir))),
        ("bin/commit.sh", COMMIT_SH.to_owned()),
        (
            ".gitignore",
            "lab-answers/\n.claude/wrap-up\n.DS_Store\n".to_owned(),
        ),
    ];
    for (path, text) in owned {
        let path = dir.join(path);
        if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            write_aside(&path, text.as_bytes())?;
        }
    }
    executable(&dir.join("bin/commit.sh"))?;
    if fresh {
        let name = team.name_of(&session.root);
        git(&dir, &["init", "-q"])?;
        commit(&dir, name, &format!("A new agent: {name}"))?;
    }
    Ok(dir)
}

/// The permission rules that let a lead call each of its reports, by key: one rule each, under
/// both names Claude Code has given its Agent tool.
#[must_use]
pub fn call_rules(reports: &[String]) -> Vec<String> {
    reports
        .iter()
        .flat_map(|key| [format!("Agent({key})"), format!("Task({key})")])
        .collect()
}

/// The agent's sandbox: Claude Code's permissions and hooks for its folder.
#[must_use]
pub fn settings(setup: &Setup, team: &Team, session: &Session, dir: &Path) -> Value {
    let mut allow: Vec<String> = ["Read(./**)", "Edit(./**)", "Glob", "Grep"]
        .map(String::from)
        .into();
    if session.lab {
        allow.push("mcp__persephone-lab".into());
    }
    let reports: Vec<String> = team
        .reports_of(&session.root)
        .iter()
        .map(|r| crate::team::key(&r.name))
        .collect();
    allow.extend(call_rules(&reports));
    let mut deny: Vec<String> = [
        "Bash",
        "WebFetch",
        "WebSearch",
        "Read(./.git/**)",
        "Edit(./.git/**)",
        "Edit(./.claude/**)",
        "Edit(./bin/**)",
        "Edit(./inbox/**)",
        "Edit(./.mcp.json)",
        "Edit(./CLAUDE.md)",
        "Edit(./principles.md)",
        "Edit(./subject.md)",
        "Edit(./vetoed.md)",
        "Read(~/.ssh/**)",
        "Read(~/.claude/**)",
        "Read(~/.claude.json)",
        "Read(~/Library/**)",
        "Read(~/Documents/**)",
        "Read(~/Desktop/**)",
        "Read(~/Downloads/**)",
    ]
    .map(String::from)
    .into();
    let mut outside: Vec<PathBuf> = setup.keep_out.clone();
    if let LabData::Own(data) = &team.lab.data {
        outside.push(data.clone());
    }
    // The rest of the team's folder and every other team's, so hand-offs are the only way
    // between agents.
    outside.extend(siblings(&setup.root, &team.id));
    outside.extend(siblings(&team_dir(&setup.root, team), &session.root));
    for path in outside {
        let rule = absolute(&path);
        deny.push(format!("Read({rule}/**)"));
        deny.push(format!("Edit({rule}/**)"));
    }
    let flag = shell_quote(&dir.join(WRAP_UP));
    let gate = format!(
        "test ! -e {flag} || {{ echo 'Your budget is used: do your end-of-session steps now, without Lab or your team.' >&2; exit 2; }}"
    );
    let commit = shell_quote(&dir.join("bin/commit.sh"));
    json!({
        "permissions": { "allow": allow, "deny": deny },
        "enableAllProjectMcpServers": true,
        "hooks": {
            "PreToolUse": [{
                "matcher": "mcp__persephone-lab__.*|Agent|Task",
                "hooks": [{ "type": "command", "command": gate }],
            }],
            "Stop": [{ "hooks": [{ "type": "command", "command": commit }] }],
        },
    })
}

/// `.mcp.json`: the Lab this session's agents use, saving big answers in its own folder; no
/// servers if none of them uses Lab.
#[must_use]
pub fn mcp(setup: &Setup, team: &Team, session: &Session, dir: &Path) -> Value {
    let Some(lab) = team.lab.program.clone().or_else(|| setup.lab.clone()) else {
        return json!({ "mcpServers": {} });
    };
    if !session.lab {
        return json!({ "mcpServers": {} });
    }
    let mut args = Vec::new();
    if let LabData::Own(data) = &team.lab.data {
        args.push("--data".to_owned());
        args.push(data.display().to_string());
    }
    args.push("--answers".into());
    args.push(dir.join("lab-answers").display().to_string());
    args.push("mcp".into());
    json!({
        "mcpServers": {
            "persephone-lab": { "command": lab.display().to_string(), "args": args },
        },
    })
}

/// The folders beside `name` in `dir`, which an agent there is kept out of.
fn siblings(dir: &Path, name: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name() != name)
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// A path as Claude Code's permission rules write an absolute one: `//Users/...`.
fn absolute(path: &Path) -> String {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    format!("//{}", path.display().to_string().trim_start_matches('/'))
}

/// A path quoted for `sh`.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Copies what's in `from` into `to`, folders and all, making `to`.
///
/// # Errors
///
/// Says what couldn't be copied.
pub fn copy_into(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("couldn't make {}: {e}", to.display()))?;
    let Ok(entries) = fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_into(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|e| format!("couldn't copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Commits everything changed in `dir`, as `who`. Says whether there was anything to commit.
///
/// # Errors
///
/// Says why git failed.
pub fn commit(dir: &Path, who: &str, message: &str) -> Result<bool, String> {
    git(dir, &["add", "-A"])?;
    let unchanged = Command::new("git")
        .current_dir(dir)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(|e| format!("couldn't run git: {e}"))?
        .success();
    if unchanged {
        return Ok(false);
    }
    git(
        dir,
        &[
            "-c",
            &format!("user.name={who}"),
            "-c",
            "user.email=agent@persephone.local",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )?;
    Ok(true)
}

fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Written beside `path` and moved into place, so nothing reads half a file.
///
/// # Errors
///
/// Says what couldn't be written.
pub fn write_aside(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}-{}.partial",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let partial = path.with_file_name(name);
    fs::File::create(&partial)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

#[cfg(unix)]
fn executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("couldn't make {} runnable: {e}", path.display()))
}

#[cfg(not(unix))]
fn executable(_: &Path) -> Result<(), String> {
    Ok(())
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

fn ensure_newline(text: &str) -> String {
    let mut text = text.trim_end().to_owned();
    text.push('\n');
    text
}

/// The local time to the minute, `2026-11-05-1432`, for runs' and archives' names.
#[must_use]
pub fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let Ok(secs) = libc::time_t::try_from(secs) else {
        return secs.to_string();
    };
    // SAFETY: `localtime_r` only writes into `tm`, which is ours.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&raw const secs, &raw mut tm) }.is_null() {
        return secs.to_string();
    }
    format!(
        "{:04}-{:02}-{:02}-{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plan::sessions, team::Starter};

    fn setup(name: &str) -> Setup {
        let root =
            std::env::temp_dir().join(format!("persephone-agents-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Setup {
            root,
            claude: "claude".into(),
            lab: Some("/opt/persephone-lab".into()),
            keep_out: vec!["/code/persephone".into()],
        }
    }

    #[test]
    fn a_new_agent_gets_the_kits_folder_in_git() {
        let setup = setup("new");
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        team.brief = "Study BTC.".into();
        let session = &sessions(&team)[0];
        let dir = refresh(&setup, &team, session).unwrap();
        assert_eq!(dir, setup.root.join(&team.id).join(&session.root));
        for file in [
            "CLAUDE.md",
            "memory/index.md",
            "memory/beliefs.md",
            "tasks.md",
            "app/bugs.md",
            ".claude/members.json",
        ] {
            assert!(dir.join(file).exists(), "{file}");
        }
        let subject = fs::read_to_string(dir.join("subject.md")).unwrap();
        assert!(subject.contains("Study BTC."));
        let members: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(".claude/members.json")).unwrap())
                .unwrap();
        assert!(members.get("analyst-a").is_some());
        let log = Command::new("git")
            .current_dir(&dir)
            .args(["log", "--format=%s %an"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "A new agent: Lead Lead"
        );
        let _ = fs::remove_dir_all(&setup.root);
    }

    #[test]
    fn a_refresh_rewrites_only_the_apps_files() {
        let setup = setup("refresh");
        let mut team = Team::new("Solo", Starter::OneAnalyst);
        team.brief = "First.".into();
        let session = sessions(&team)[0].clone();
        let dir = refresh(&setup, &team, &session).unwrap();
        fs::write(dir.join("memory/beliefs.md"), "learned").unwrap();
        fs::write(dir.join("app/ideas.md"), "an idea").unwrap();
        fs::write(dir.join("runs/table.csv"), "1,2").unwrap();
        fs::write(dir.join("vetoed.md"), "not that").unwrap();
        team.brief = "Second.".into();
        team.agents[0].name = "Renamed".into();
        let again = refresh(&setup, &team, &session).unwrap();
        assert_eq!(again, dir, "renaming keeps the folder");
        assert_eq!(
            fs::read_to_string(dir.join("memory/beliefs.md")).unwrap(),
            "learned"
        );
        assert_eq!(
            fs::read_to_string(dir.join("app/ideas.md")).unwrap(),
            "an idea"
        );
        assert_eq!(
            fs::read_to_string(dir.join("runs/table.csv")).unwrap(),
            "1,2"
        );
        assert_eq!(
            fs::read_to_string(dir.join("vetoed.md")).unwrap(),
            "not that"
        );
        assert!(
            fs::read_to_string(dir.join("subject.md"))
                .unwrap()
                .contains("Second.")
        );
        assert!(
            fs::read_to_string(dir.join("CLAUDE.md"))
                .unwrap()
                .contains("You are Renamed")
        );
        let _ = fs::remove_dir_all(&setup.root);
    }

    #[test]
    fn the_sandbox_keeps_out_code_data_and_the_other_agents() {
        let setup = setup("sandbox");
        let mut team = Team::new("Pair", Starter::ResearcherAndReviewer);
        team.brief = "Study.".into();
        let all = sessions(&team);
        let first = refresh(&setup, &team, &all[0]).unwrap();
        let second = refresh(&setup, &team, &all[1]).unwrap();
        // Refreshed again, now that its fellow agent's folder is there.
        refresh(&setup, &team, &all[0]).unwrap();
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(first.join(".claude/settings.json")).unwrap())
                .unwrap();
        let deny: Vec<&str> = settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(deny.contains(&"Bash"));
        assert!(deny.contains(&"Read(//code/persephone/**)"), "{deny:?}");
        let second = absolute(&second);
        assert!(
            deny.contains(&format!("Read({second}/**)").as_str()),
            "{deny:?}"
        );
        assert!(
            !deny.iter().any(|d| d.contains(&absolute(&first))),
            "not its own"
        );
        let hook = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(hook.contains(".claude/wrap-up"));
        let mcp: Value =
            serde_json::from_str(&fs::read_to_string(first.join(".mcp.json")).unwrap()).unwrap();
        let args = mcp["mcpServers"]["persephone-lab"]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args.last().unwrap(), "mcp");
        assert!(
            args.iter()
                .any(|a| a.as_str().unwrap().ends_with("lab-answers"))
        );
        let _ = fs::remove_dir_all(&setup.root);
    }

    #[test]
    fn teams_save_load_and_archive() {
        let setup = setup("teams");
        let a = Team::new("Beta", Starter::OneAnalyst);
        let b = Team::new("alpha", Starter::Blank);
        save_team(&setup.root, &a).unwrap();
        save_team(&setup.root, &b).unwrap();
        let names: Vec<String> = load_teams(&setup.root)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["alpha", "Beta"]);
        let moved = archive(&setup.root, &team_dir(&setup.root, &a), &a.id).unwrap();
        assert!(moved.join("team.json").exists());
        assert_eq!(load_teams(&setup.root).len(), 1);
        let _ = fs::remove_dir_all(&setup.root);
    }

    #[test]
    fn stamps_are_to_the_minute() {
        let stamp = stamp();
        assert_eq!(stamp.len(), 15, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
    }
}
