use super::*;
use anyhow::Context;
use grit_lib::{
    command_runner::{CommandRunner, CommandSpec, RunningCommand, system_command_runner},
    config::ConfigSet,
    diff::{DiffEntry, DiffOptions, DiffRequest, DiffStatus, resolve_diff_request},
    objects::{FileMode, ObjectId},
    porcelain::{
        commit::{CommitRequest, CommitScope, commit_with_scope},
        status::{StatusModel, StatusOptions, UntrackedMode, status, untracked_mode_from_config},
    },
    prelude::{Environment, InitOptions, Repository, RepositoryOptions},
    progress::{CancelToken, NullProgress},
    refs::{read_symbolic_ref_for_repository, store::RefNameRef},
    remote::{
        HttpClientFactory, Remote, RemoteError, RemoteResult, RemoteUrl,
        clone::{CloneIntoOptions, clone_into},
        credentials::{Credential, CredentialProvider, HelperCredentialProvider},
        http_client::{HttpClient, UreqHttpClient},
        transfer::{FetchOptions, PushOptions, PushRefSpec},
    },
    repo_handles::RemoteUrlKind,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

pub(crate) fn folder(link: &KicadProject) -> Option<PathBuf> {
    let project = Path::new(&link.path);
    project
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_owned)
}

fn environment(path: &Path) -> Environment {
    // Load user configuration without inheriting Git overrides that could redirect
    // an operation to another worktree, index, or object store.
    let vars = std::env::vars_os().filter(|(key, _)| {
        matches!(
            key.to_str(),
            Some(
                "HOME"
                    | "XDG_CONFIG_HOME"
                    | "USERPROFILE"
                    | "HOMEDRIVE"
                    | "HOMEPATH"
                    | "ProgramFiles"
                    | "PATH"
                    | "GIT_AUTHOR_NAME"
                    | "GIT_AUTHOR_EMAIL"
                    | "GIT_COMMITTER_NAME"
                    | "GIT_COMMITTER_EMAIL"
            )
        )
    });
    Environment::from_vars(vars, path.to_path_buf())
}

pub(crate) fn open(path: &Path) -> Result<Option<Repository>> {
    open_with_ssh(path, SshSettings::default())
}

fn open_with_ssh(path: &Path, ssh: SshSettings) -> Result<Option<Repository>> {
    match path.join(".git").symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let options = RepositoryOptions::with_environment(environment(path))
        .with_command_runner(Arc::new(GritCommandRunner { ssh }));
    let repo = Repository::discover(Some(path), &options)?;
    let root = repo
        .work_tree()
        .context("Git repository has no working folder")?;
    ensure!(
        root.canonicalize()? == path.canonicalize()?,
        "Git working folder does not match the project folder"
    );
    Ok(Some(repo))
}

pub(super) fn configured_remote(path: &Path) -> Result<GitRemote> {
    let Some(repo) = open(path)? else {
        return Ok(GitRemote::default());
    };
    remote_from_repo(&repo)
}

fn remote_from_repo(repo: &Repository) -> Result<GitRemote> {
    let remotes = repo.remotes().configured()?;
    Ok(remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .or_else(|| remotes.first())
        .map(|remote| GitRemote {
            name: remote.name.clone(),
            url: remote.url.clone().unwrap_or_default(),
        })
        .unwrap_or_default())
}

pub(super) fn validate_remote(remote: &mut GitRemote) -> Result<()> {
    remote.name = remote.name.trim().into();
    remote.url = remote.url.trim().into();
    ensure!(!remote.name.is_empty(), "Remote name is required");
    grit_lib::refs::store::RefName::try_from(
        format!("refs/remotes/{}/branch", remote.name).as_str(),
    )?;
    ensure!(
        !remote.url.contains(['\n', '\r', '\0']),
        "Remote URL must be a single line"
    );
    if !remote.url.is_empty() {
        RemoteUrl::try_from(remote.url.as_str())?;
    }
    if remote.url.contains("://")
        && let Ok(url) = url::Url::parse(&remote.url)
    {
        ensure!(
            url.password().is_none()
                && (!matches!(url.scheme(), "http" | "https")
                    || (url.username().is_empty()
                        && url.query().is_none()
                        && url.fragment().is_none())),
            "Remote URLs are synced. Put credentials in Version control preferences"
        );
    }
    Ok(())
}

pub(super) fn configure(path: &Path, remote: &GitRemote) -> Result<()> {
    ensure!(path.is_dir(), "Project folder does not exist");
    let mut remote = remote.clone();
    validate_remote(&mut remote)?;
    let repo = match open(path)? {
        Some(repo) => repo,
        None => {
            let mut options = InitOptions::default();
            options.environment = Some(environment(path));
            let repo = Repository::init(path, &options)?.0;
            add_default_ignores(path)?;
            repo
        }
    };
    set_remote(&repo, &remote)
}

fn set_remote(repo: &Repository, remote: &GitRemote) -> Result<()> {
    if remote.url.is_empty() {
        return Ok(());
    }
    let remotes = repo.remotes();
    if let Some(current) = remotes
        .configured()?
        .into_iter()
        .find(|item| item.name == remote.name)
    {
        if current.url.as_deref() != Some(&remote.url) {
            remotes.set_url(&remote.name, &remote.url, RemoteUrlKind::Fetch, false)?;
        }
    } else {
        remotes.add(&remote.name, &remote.url)?;
    }
    Ok(())
}

fn add_default_ignores(folder: &Path) -> Result<()> {
    use std::io::Write;
    let path = folder.join(".gitignore");
    let existing = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(metadata.is_file(), ".gitignore must be a regular file");
            std::fs::read(&path)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let rules = [crate::project_cache::NAME, ".DS_Store", ".history/", "*.lck"];
    let missing = rules
        .iter()
        .filter(|rule| {
            !existing
                .split(|byte| *byte == b'\n')
                .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == rule.as_bytes())
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        if !existing.is_empty() && !existing.ends_with(b"\n") {
            writeln!(file)?;
        }
        for rule in missing {
            writeln!(file, "{rule}")?;
        }
    }
    Ok(())
}

fn agent_preference_path() -> Result<PathBuf> {
    Ok(crate::data_location::preference_path()?.with_file_name("ssh-agent.json"))
}

fn default_agent_socket() -> String {
    if let Ok(socket) = std::env::var("SSH_AUTH_SOCK")
        && !socket.is_empty()
    {
        return socket;
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/bin/launchctl")
        .args(["getenv", "SSH_AUTH_SOCK"])
        .output()
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    String::new()
}

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(super) struct SshSettings {
    pub socket: String,
    pub private_key: String,
    pub public_key: String,
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct HttpsToken {
    pub host: String,
    pub username: String,
    pub token: String,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct GitPreferences {
    pub preserve_bundles: bool,
    pub(super) ssh: SshSettings,
    pub(super) tokens: Vec<HttpsToken>,
}

fn git_preferences_path() -> Result<PathBuf> {
    Ok(crate::data_location::preference_path()?.with_file_name("version-control.json"))
}

pub(crate) fn load_preferences() -> Result<GitPreferences> {
    let mut preferences = match std::fs::read(git_preferences_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let socket = match std::fs::read(agent_preference_path()?) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error.into()),
            };
            GitPreferences {
                ssh: SshSettings {
                    socket,
                    ..Default::default()
                },
                ..Default::default()
            }
        }
        Err(error) => return Err(error.into()),
    };
    if preferences.ssh.socket.is_empty() {
        preferences.ssh.socket = default_agent_socket();
    }
    Ok(preferences)
}

pub(super) fn save_preferences(preferences: &mut GitPreferences) -> Result<()> {
    use std::io::Write;
    let ssh = &mut preferences.ssh;
    for field in [&mut ssh.socket, &mut ssh.private_key, &mut ssh.public_key] {
        *field = field.trim().to_owned();
        ensure!(
            field.is_empty() || Path::new(field).is_absolute(),
            "Enter absolute paths for SSH settings or leave them blank"
        );
    }
    let mut hosts = std::collections::BTreeSet::new();
    for entry in &mut preferences.tokens {
        let host = entry.host.trim();
        let url =
            url::Url::parse(&format!("https://{host}")).context("Enter a valid HTTPS host")?;
        ensure!(
            !host.contains(['/', '\\', '@', '?', '#'])
                && !entry.username.trim().is_empty()
                && !entry.token.trim().is_empty(),
            "Each HTTPS entry needs a host, username, and token"
        );
        entry.host = https_host(url.as_str()).context("Enter a valid HTTPS host")?;
        entry.username = entry.username.trim().to_owned();
        entry.token = entry.token.trim().to_owned();
        ensure!(
            hosts.insert(entry.host.clone()),
            "Use one token per HTTPS host"
        );
    }

    let path = git_preferences_path()?;
    let folder = path.parent().context("No configuration directory")?;
    std::fs::create_dir_all(folder)?;
    // NamedTempFile uses owner-only permissions on Unix and is atomically replaced.
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    serde_json::to_writer(&mut file, preferences)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn https_host(remote_url: &str) -> Option<String> {
    let url = url::Url::parse(remote_url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

struct GritCommandRunner {
    ssh: SshSettings,
}

impl CommandRunner for GritCommandRunner {
    fn spawn(&self, spec: &CommandSpec) -> std::io::Result<RunningCommand> {
        // Grit can probe `git --exec-path` while locating HTTPS credential helpers.
        if spec
            .program
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case("git"))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Git executable is disabled",
            ));
        }
        let mut spec = spec.clone();
        if !self.ssh.socket.is_empty() {
            spec.env
                .set
                .push(("SSH_AUTH_SOCK".into(), self.ssh.socket.clone().into()));
        }
        if spec
            .program
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case("ssh"))
        {
            let identities = [&self.ssh.private_key, &self.ssh.public_key];
            let args = identities
                .into_iter()
                .filter(|path| !path.is_empty())
                .flat_map(|path| [std::ffi::OsString::from("-i"), path.into()]);
            if !self.ssh.private_key.is_empty() || !self.ssh.public_key.is_empty() {
                spec.args.splice(
                    ..0,
                    ["-o".into(), "IdentitiesOnly=yes".into()]
                        .into_iter()
                        .chain(args),
                );
            }
        }
        system_command_runner().spawn(&spec)
    }
}

struct PushCredentials {
    token: Option<HttpsToken>,
    helpers: HelperCredentialProvider,
}

impl PushCredentials {
    fn token_for(&self, credential: &Credential) -> Option<&HttpsToken> {
        self.token.as_ref().filter(|token| {
            credential.protocol.as_deref() == Some("https")
                && credential
                    .host
                    .as_deref()
                    .is_some_and(|host| host.eq_ignore_ascii_case(&token.host))
        })
    }
}

impl CredentialProvider for PushCredentials {
    fn fill(&self, input: &Credential) -> grit_lib::error::Result<Credential> {
        let Some(token) = self.token_for(input) else {
            return self.helpers.fill(input);
        };
        let mut credential = input.clone();
        credential.username = Some(token.username.clone());
        credential.password = Some(token.token.clone());
        Ok(credential)
    }

    fn approve(&self, credential: &Credential) -> grit_lib::error::Result<()> {
        if self.token_for(credential).is_some() {
            Ok(())
        } else {
            self.helpers.approve(credential)
        }
    }

    fn reject(&self, credential: &Credential) -> grit_lib::error::Result<()> {
        if self.token_for(credential).is_some() {
            Ok(())
        } else {
            self.helpers.reject(credential)
        }
    }
}

struct GritHttpClientFactory<'a> {
    environment: Environment,
    runner: Arc<dyn CommandRunner>,
    token: Option<&'a HttpsToken>,
}

impl HttpClientFactory for GritHttpClientFactory<'_> {
    fn create(&self, config: &ConfigSet) -> RemoteResult<Box<dyn HttpClient>> {
        let credentials = PushCredentials {
            token: self.token.cloned(),
            helpers: HelperCredentialProvider::new(config.clone())
                .with_command_runner(self.runner.clone()),
        };
        let client = UreqHttpClient::from_config_with_environment(config, &self.environment)
            .map_err(|error| RemoteError::InvalidUrl(error.to_string()))?
            .with_credential_provider(Box::new(credentials));
        Ok(Box::new(client))
    }

    fn create_for_preflight(&self, config: &ConfigSet) -> RemoteResult<Box<dyn HttpClient>> {
        // Keep credentials intact during best-effort probes.
        UreqHttpClient::from_config_with_environment(config, &self.environment)
            .map(|client| Box::new(client) as Box<dyn HttpClient>)
            .map_err(|error| RemoteError::InvalidUrl(error.to_string()))
    }
}

fn current_branch(repo: &Repository) -> Result<Option<String>> {
    Ok(read_symbolic_ref_for_repository(repo, "HEAD")?
        .filter(|branch| branch.starts_with("refs/heads/")))
}

fn push_target(repo: &Repository, remote: &GitRemote) -> Result<(Remote, String)> {
    ensure!(
        !remote.url.is_empty(),
        "Set a remote URL or path before pushing"
    );
    let remote = repo.remotes().get(&remote.name)?;
    let branch = current_branch(repo)?.context("Check out a branch before pushing")?;
    Ok((remote, branch))
}

fn has_unpushed_commits(repo: &Repository, remote: &GitRemote) -> Result<bool> {
    if remote.url.is_empty() {
        return Ok(false);
    }
    let Some(branch) = current_branch(repo)? else {
        return Ok(false);
    };
    let branch_ref = RefNameRef::from_trusted(&branch);
    if repo.ref_store().read_raw(branch_ref)?.is_none() {
        return Ok(false);
    }
    let tracking = format!(
        "refs/remotes/{}/{}",
        remote.name,
        &branch["refs/heads/".len()..]
    );
    let tracking_ref = RefNameRef::from_trusted(&tracking);
    if repo.ref_store().read_raw(tracking_ref)?.is_none() {
        return Ok(true);
    }
    Ok(!repo.revwalk().is_ancestor(&branch, &tracking)?)
}

fn push(folder: &Path, remote: &GitRemote) -> Result<()> {
    let mut remote = remote.clone();
    validate_remote(&mut remote)?;
    let preferences = load_preferences()?;
    let repo = open_with_ssh(folder, preferences.ssh)?.context("Git repository is missing")?;
    set_remote(&repo, &remote)?;
    let (remote, branch) = push_target(&repo, &remote)?;
    let head = repo
        .references()
        .resolve(RefNameRef::from_trusted(&branch))?;
    let client = http_client(&repo, remote.push_url(), &preferences.tokens);
    let outcome = remote.push(
        &repo,
        &[PushRefSpec {
            src: Some(head),
            src_ref: Some(branch.clone()),
            dst: branch,
            force: false,
            delete: false,
            expected_old: None,
            expect_absent: false,
            hook_src_ref: None,
        }],
        PushOptions::default(),
        &mut NullProgress,
        &CancelToken::new(),
        Some(&client),
    )?;
    if let Some(rejected) = outcome
        .results
        .iter()
        .find(|result| result.status.is_error())
    {
        anyhow::bail!(
            "Push rejected for {}: {}",
            rejected.remote_ref,
            rejected
                .message
                .clone()
                .unwrap_or_else(|| format!("{:?}", rejected.status)),
        );
    }
    Ok(())
}

fn http_client<'a>(
    repo: &Repository,
    remote: &RemoteUrl,
    tokens: &'a [HttpsToken],
) -> GritHttpClientFactory<'a> {
    let token = match remote {
        RemoteUrl::Https(url) => {
            https_host(url).and_then(|host| tokens.iter().find(|entry| entry.host == host))
        }
        _ => None,
    };
    GritHttpClientFactory {
        environment: repo.environment().clone(),
        runner: repo.command_runner(),
        token,
    }
}

pub(super) fn remote_fields(ui: &mut egui::Ui, remote: &mut GitRemote) {
    surfaces::row(ui, "Git remote name", |ui| {
        surfaces::field(ui, &mut remote.name)
    });
    surfaces::row(ui, "Git remote URL or path", |ui| {
        surfaces::field(ui, &mut remote.url)
    });
}

fn fetch(path: &Path, remote: &GitRemote) -> Result<Option<KicadProject>> {
    let mut remote = remote.clone();
    validate_remote(&mut remote)?;
    ensure!(
        !remote.url.is_empty(),
        "Set a remote URL or path before fetching"
    );
    let preferences = load_preferences()?;
    if let Some(repo) = open_with_ssh(path, preferences.ssh.clone())? {
        set_remote(&repo, &remote)?;
        let source = repo.remotes().get(&remote.name)?;
        source.fetch(
            &repo,
            FetchOptions::default(),
            &mut NullProgress,
            &CancelToken::new(),
            Some(&http_client(&repo, source.fetch_url(), &preferences.tokens)),
        )?;
        return Ok(None);
    }

    ensure!(!path.exists(), "Choose a new folder for the Git checkout");
    let parent = path.parent().context("Choose a destination folder")?;
    let staging = tempfile::tempdir_in(parent)?;
    let checkout = staging.path().join("checkout");
    let source = RemoteUrl::try_from(remote.url.as_str())?;
    ensure!(
        !matches!(&source, RemoteUrl::Local(source) if source.is_relative()),
        "Cloning from a local folder requires an absolute remote path"
    );
    let mut init = InitOptions::default();
    init.environment = Some(environment(&checkout));
    Repository::init(&checkout, &init)?;
    let repo = open_with_ssh(&checkout, preferences.ssh)?.context("Git checkout is missing")?;
    let mut options = CloneIntoOptions::new(&remote.url).with_environment(environment(&checkout));
    options.remote_name = remote.name;
    let client = http_client(&repo, &source, &preferences.tokens);
    clone_into(
        repo,
        options,
        &mut NullProgress,
        &CancelToken::new(),
        Some(&client),
    )?;

    let mut projects = Vec::new();
    for entry in std::fs::read_dir(&checkout)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "kicad_pro")
        {
            projects.push(entry.file_name());
        }
    }
    ensure!(!path.exists(), "The checkout destination already exists");
    std::fs::rename(&checkout, path)?;
    Ok(if projects.len() == 1 {
        Some(KicadProject {
            path: path.join(&projects[0]).to_string_lossy().into_owned(),
            ..Default::default()
        })
    } else {
        None
    })
}

fn status_of(repo: &Repository) -> Result<StatusModel> {
    let config = repo.config().snapshot()?;
    let untracked = match untracked_mode_from_config(&config)? {
        UntrackedMode::No => UntrackedMode::No,
        _ => UntrackedMode::All,
    };
    let options = StatusOptions {
        untracked,
        ahead_behind: false,
        write_index: false,
        ..Default::default()
    };
    Ok(status(
        repo,
        &options,
        &mut NullProgress,
        &CancelToken::new(),
    )?)
}

fn has_changes(repo: &Repository) -> Result<bool> {
    let status = status_of(repo)?;
    if !status.untracked.is_empty() || !status.conflicts.is_empty() {
        return Ok(true);
    }
    if status.is_clean() {
        return Ok(false);
    }
    let diff = resolve_diff_request(repo, None, &DiffRequest::default())?;
    let changes = repo
        .diff()
        .entries(&diff.target, &diff.pathspec, &DiffOptions::default())?;
    Ok(!changes.entries.is_empty())
}

#[derive(Clone, PartialEq)]
struct Change {
    path: String,
    kind: &'static str,
    lines: Option<(usize, usize)>,
}

fn changes(path: &Path) -> Result<Vec<Change>> {
    let repo = open(path)?.context("Git repository is missing")?;
    let status = status_of(&repo)?;
    ensure!(
        status.conflicts.is_empty(),
        "Resolve Git conflicts before committing"
    );
    let diff = resolve_diff_request(&repo, None, &DiffRequest::default())?;
    let options = DiffOptions::default();
    let mut worktree = repo.diff().uncommitted()?;
    let mut entries = worktree
        .entries(&diff.target, &diff.pathspec, &options)?
        .entries;
    let tracked: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry.path().to_owned())
        .collect();
    let embedded: BTreeSet<_> = status.embedded_repositories.into_iter().collect();
    for name in status.untracked {
        if tracked.contains(&name) {
            continue;
        }
        let mode = if embedded.contains(&name) {
            FileMode::GITLINK
        } else if path.join(&name).symlink_metadata()?.is_symlink() {
            FileMode::SYMLINK
        } else {
            FileMode::REGULAR
        };
        entries.push(DiffEntry {
            status: DiffStatus::Added,
            old_path: None,
            new_path: Some(name),
            old_mode: FileMode::ABSENT,
            new_mode: mode,
            old_oid: ObjectId::zero(),
            new_oid: ObjectId::zero(),
            score: None,
        });
    }

    let mut changes: Vec<_> = worktree
        .load_worktree_changes(entries)?
        .into_iter()
        .map(|file| {
            let kind = match file.status {
                DiffStatus::Added | DiffStatus::Copied => "Added",
                DiffStatus::Deleted => "Removed",
                DiffStatus::Renamed => "Renamed",
                _ => "Modified",
            };
            let lines = if file.old_mode == FileMode::GITLINK || file.new_mode == FileMode::GITLINK
            {
                None
            } else {
                file.line_stat(&options)
                    .map(|stat| (stat.insertions, stat.deletions))
            };
            let path = match file.old_path {
                Some(old) => format!("{old} → {}", file.path),
                None => file.path,
            };
            Change { path, kind, lines }
        })
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

fn background<T: Send + 'static>(
    ctx: &egui::Context,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Receiver<Result<T>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let _ = crate::async_io::spawn(async move {
        let _ = tx.send(crate::async_io::blocking(work).await);
        ctx.request_repaint();
    });
    rx
}

enum RepoState {
    Missing,
    Ready(bool),
    PushReady,
    Error(String),
}

struct Scan {
    state: Option<RepoState>,
    pending: Option<Receiver<Result<RepoState>>>,
    next: Instant,
}

#[derive(Default)]
pub(super) struct ProjectGit {
    opened_project: Option<String>,
    scans: BTreeMap<PathBuf, Scan>,
    dialog: Option<GitDialog>,
}

struct GitDialog {
    project: String,
    project_id: String,
    remote: GitRemote,
    path: PathBuf,
    action: GitAction,
    text: String,
    error: Option<String>,
    pending: Option<Receiver<Result<GitOutcome>>>,
}

enum GitOutcome {
    Initialized(GitRemote),
    Fetched(Option<KicadProject>),
    Review(Vec<Change>),
    Refreshed(Vec<Change>),
    Saved,
    PushFailed(String),
}

enum GitAction {
    Init,
    Fetch,
    FetchComplete,
    Push,
    Commit(Vec<Change>),
}

impl ProjectGit {
    pub(super) fn open_project(&mut self, name: &str) {
        if !self.is_open_project(name) {
            self.opened_project = Some(name.to_owned());
            self.scans.clear();
        }
    }

    pub(super) fn is_open_project(&self, name: &str) -> bool {
        self.opened_project.as_deref() == Some(name)
    }

    pub(super) fn invalidate(&mut self, path: &Path) {
        self.scans.remove(path);
    }

    pub(super) fn control(
        &mut self,
        ui: &mut egui::Ui,
        path: PathBuf,
        project: &Project,
        rect: egui::Rect,
    ) {
        let scan = self.scans.entry(path.clone()).or_insert_with(|| Scan {
            state: None,
            pending: None,
            next: Instant::now(),
        });
        if let Some(pending) = &scan.pending {
            match pending.try_recv() {
                Ok(state) => {
                    scan.state =
                        Some(state.unwrap_or_else(|error| RepoState::Error(error.to_string())));
                    scan.pending = None;
                    scan.next = Instant::now() + Duration::from_secs(2);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    scan.state = Some(RepoState::Error("Git status check stopped".into()));
                    scan.pending = None;
                    scan.next = Instant::now() + Duration::from_secs(2);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if scan.pending.is_none() && Instant::now() >= scan.next {
            let path = path.clone();
            let remote = project.git_remote.clone();
            scan.pending = Some(background(ui.ctx(), move || {
                let Some(repo) = open(&path)? else {
                    return Ok(RepoState::Missing);
                };
                if has_changes(&repo)? {
                    return Ok(RepoState::Ready(true));
                }
                let remote = match remote {
                    Some(remote) => remote,
                    None => remote_from_repo(&repo)?,
                };
                if has_unpushed_commits(&repo, &remote)? {
                    return Ok(RepoState::PushReady);
                }
                Ok(RepoState::Ready(false))
            }));
        }
        ui.ctx().request_repaint_after(Duration::from_secs(2));
        let (icon, clickable, active, tip) = match &scan.state {
            Some(RepoState::PushReady) => (
                Icon::GitPush,
                true,
                true,
                "Push commits to the project remote",
            ),
            Some(RepoState::Missing) => (Icon::Git, true, false, "Initialize Git"),
            Some(RepoState::Ready(true)) => (Icon::Commit, true, true, "Review changes and commit"),
            Some(RepoState::Ready(false)) => (Icon::Commit, false, false, "Nothing to commit"),
            Some(RepoState::Error(error)) => (Icon::Git, false, false, error.as_str()),
            None => return,
        };
        let response = ui.interact(
            rect,
            ui.id().with(("project_git", &project.id)),
            if clickable {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        theme::paint_icon(
            ui.painter(),
            icon,
            rect.shrink(5.),
            if active || (clickable && response.hovered()) {
                GOLD
            } else {
                theme::MUTED
            },
        );
        if response.on_hover_text(tip).clicked() {
            let remote = project
                .git_remote
                .clone()
                .map(Ok)
                .unwrap_or_else(|| configured_remote(&path));
            let (remote, error) = match remote {
                Ok(remote) => (remote, None),
                Err(error) => (GitRemote::default(), Some(error.to_string())),
            };
            let (action, pending) = if matches!(scan.state, Some(RepoState::Missing)) {
                (GitAction::Init, None)
            } else if matches!(scan.state, Some(RepoState::PushReady)) {
                let path = path.clone();
                let remote = remote.clone();
                (
                    GitAction::Push,
                    Some(background(ui.ctx(), move || {
                        push(&path, &remote)?;
                        Ok(GitOutcome::Saved)
                    })),
                )
            } else {
                let path = path.clone();
                let pending = background(ui.ctx(), move || changes(&path).map(GitOutcome::Review));
                (GitAction::Commit(vec![]), Some(pending))
            };
            self.dialog = Some(GitDialog {
                project: project.name.clone(),
                project_id: project.id.clone(),
                remote,
                path,
                action,
                error,
                pending,
                text: String::new(),
            });
        }
    }

    pub(super) fn fetch_dialog(
        &mut self,
        project: &Project,
        path: Option<PathBuf>,
        remote: GitRemote,
    ) -> bool {
        let path = path.filter(|path| path.join(".git").exists()).or_else(|| {
            rfd::FileDialog::new()
                .set_title("Choose a new folder for the Git checkout")
                .save_file()
        });
        if let Some(path) = path {
            self.dialog = Some(GitDialog {
                project: project.name.clone(),
                project_id: project.id.clone(),
                remote,
                path,
                action: GitAction::Fetch,
                error: None,
                pending: None,
                text: String::new(),
            });
            return true;
        }
        false
    }

    pub(super) fn window(&mut self, ctx: &egui::Context, store: &SharedStore) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        if let Some(pending) = &dialog.pending {
            let result = match pending.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(anyhow::anyhow!("Git operation stopped")))
                }
            };
            if let Some(result) = result {
                dialog.pending = None;
                match result {
                    Ok(GitOutcome::Initialized(remote)) => {
                        let result = store.lock().unwrap().change(|data| {
                            let project = data
                                .projects
                                .iter_mut()
                                .find(|p| p.id == dialog.project_id)
                                .context("Project no longer exists")?;
                            project.git_remote = Some(remote);
                            Ok(())
                        });
                        self.invalidate(&dialog.path);
                        match result {
                            Ok(()) => return,
                            Err(error) => dialog.error = Some(error.to_string()),
                        }
                    }
                    Ok(GitOutcome::Fetched(link)) => {
                        self.invalidate(&dialog.path);
                        dialog.action = GitAction::FetchComplete;
                        if let Some(link) = link {
                            let result = store.lock().unwrap().change(|data| {
                                let project = data
                                    .projects
                                    .iter_mut()
                                    .find(|p| p.id == dialog.project_id)
                                    .context("Project no longer exists")?;
                                if project.kicad.is_none() {
                                    project.kicad = Some(link);
                                }
                                Ok(())
                            });
                            if let Err(error) = result {
                                dialog.error = Some(format!(
                                    "Repository fetched. Could not attach the KiCad project: {error}"
                                ));
                            }
                        }
                    }
                    Ok(GitOutcome::Saved) => {
                        self.invalidate(&dialog.path);
                        return;
                    }
                    Ok(GitOutcome::Review(files)) => dialog.action = GitAction::Commit(files),
                    Ok(GitOutcome::Refreshed(files)) => {
                        dialog.action = GitAction::Commit(files);
                        dialog.error = Some(
                            "Project changes were refreshed. Review them before committing.".into(),
                        );
                    }
                    Ok(GitOutcome::PushFailed(error)) => {
                        self.invalidate(&dialog.path);
                        dialog.action = GitAction::Push;
                        dialog.error = Some(format!("Commit saved locally. Push failed: {error}"));
                    }
                    Err(error) => {
                        self.invalidate(&dialog.path);
                        dialog.error = Some(error.to_string());
                    }
                }
            }
        }
        let busy = dialog.pending.is_some();
        let response = surfaces::sheet("project_git_dialog").show(ctx, |ui| {
            surfaces::configure(ui, 500.);
            let (label, enabled) = match &dialog.action {
                GitAction::Init => {
                    surfaces::heading(
                        ui,
                        &format!("Initialize Git — {}", dialog.project),
                        Icon::Git,
                    );
                    ui.label(format!(
                        "Create a Git repository in {}?",
                        dialog.path.display()
                    ));
                    remote_fields(ui, &mut dialog.remote);
                    surfaces::hint(ui, "Optional. Leave blank to use Git locally.");
                    ("Initialize", true)
                }
                GitAction::Fetch => {
                    surfaces::heading(ui, &format!("Fetch — {}", dialog.project), Icon::Import);
                    ui.label(format!(
                        "Fetch {} into {}.",
                        dialog.remote.name,
                        dialog.path.display()
                    ));
                    ("Fetch", true)
                }
                GitAction::FetchComplete => {
                    surfaces::heading(ui, &format!("Fetch — {}", dialog.project), Icon::Import);
                    ui.label(format!(
                        "Repository fetched into {}.",
                        dialog.path.display()
                    ));
                    ("Done", true)
                }
                GitAction::Push => {
                    surfaces::heading(ui, &format!("Push — {}", dialog.project), Icon::GitPush);
                    ui.label(format!(
                        "Push the current branch to {}.",
                        dialog.remote.name
                    ));
                    ("Retry push", true)
                }
                GitAction::Commit(files) => {
                    surfaces::heading(ui, &format!("Commit — {}", dialog.project), Icon::Commit);
                    if files.is_empty() && dialog.error.is_none() && !busy {
                        ui.label("Nothing to commit");
                    }
                    if !files.is_empty() {
                        let changes_width = ui.available_width() * 0.36;
                        TableBuilder::new(ui)
                            .id_salt("git_change_table")
                            .max_scroll_height(232.)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::remainder().clip(true))
                            .column(Column::exact(changes_width).clip(true))
                            .header(24., |mut row| {
                                row.col(|ui| {
                                    ui.strong("Name");
                                });
                                row.col(|ui| {
                                    ui.strong("Changes");
                                });
                            })
                            .body(|body| {
                                body.rows(23., files.len(), |mut row| {
                                    let file = &files[row.index()];
                                    let color = if file.kind == "Removed" {
                                        Color32::LIGHT_RED
                                    } else {
                                        theme::GREEN
                                    };
                                    row.col(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&file.path).size(12.).color(color),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&file.path);
                                    });
                                    row.col(|ui| {
                                        ui.label(RichText::new(file.kind).size(12.));
                                        if let Some((added, removed)) = file.lines {
                                            ui.label(RichText::new(format!("+{added}")).size(12.).color(theme::GREEN));
                                            ui.label(RichText::new(format!("−{removed}")).size(12.).color(Color32::LIGHT_RED));
                                        }
                                    });
                                });
                            });
                    }
                    surfaces::row(ui, "Commit message", |ui| {
                        surfaces::field(ui, &mut dialog.text);
                    });
                    ("Commit", !files.is_empty())
                }
            };
            surfaces::error(ui, &dialog.error);
            if busy {
                ui.spinner();
            }
            let enabled = enabled
                && (!matches!(dialog.action, GitAction::Commit(_))
                    || !dialog.text.trim().is_empty());
            ui.add_enabled_ui(!busy, |ui| {
                if matches!(dialog.action, GitAction::Commit(_)) {
                    ui.add_space(8.);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button = |label| {
                            egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                .fill(theme::PURPLE)
                                .corner_radius(5)
                                .min_size(egui::vec2(90., 28.))
                        };
                        let push_after = ui.add_enabled(enabled, button("Commit & Push")).clicked();
                        let commit = ui.add_enabled(enabled, button("Commit")).clicked();
                        let cancel = ui
                            .add(
                                egui::Button::new("Cancel")
                                    .fill(theme::RAISED)
                                    .corner_radius(5)
                                    .min_size(egui::vec2(75., 28.)),
                            )
                            .clicked();
                        (commit || push_after, cancel, push_after)
                    })
                    .inner
                } else {
                    let (confirm, cancel) = surfaces::actions(ui, label, enabled);
                    (confirm, cancel, false)
                }
            })
            .inner
        });
        let (confirm, cancel, push_after) = response.inner;
        if confirm && !busy {
            if matches!(dialog.action, GitAction::FetchComplete) {
                return;
            }
            let path = dialog.path.clone();
            let text = dialog.text.trim().to_owned();
            let push_only = matches!(dialog.action, GitAction::Push);
            let fetch_only = matches!(dialog.action, GitAction::Fetch);
            let mut remote = dialog.remote.clone();
            let reviewed = match &dialog.action {
                GitAction::Commit(files) => Some(files.clone()),
                _ => None,
            };
            dialog.error = None;
            dialog.pending = Some(background(ctx, move || {
                if fetch_only {
                    return fetch(&path, &remote).map(GitOutcome::Fetched);
                }
                if push_only {
                    push(&path, &remote)?;
                } else if let Some(reviewed) = reviewed {
                    let current = changes(&path)?;
                    if current != reviewed {
                        return Ok(GitOutcome::Refreshed(current));
                    }
                    let repo = open(&path)?.context("Git repository is missing")?;
                    if push_after {
                        validate_remote(&mut remote)?;
                        set_remote(&repo, &remote)?;
                        push_target(&repo, &remote)?;
                    }
                    commit_with_scope(
                        &repo,
                        &CommitRequest::new(text),
                        CommitScope::AutoStage,
                        &mut NullProgress,
                        &CancelToken::new(),
                    )?;
                    if push_after && let Err(error) = push(&path, &remote) {
                        return Ok(GitOutcome::PushFailed(error.to_string()));
                    }
                } else {
                    validate_remote(&mut remote)?;
                    configure(&path, &remote)?;
                    return Ok(GitOutcome::Initialized(remote));
                }
                Ok(GitOutcome::Saved)
            }));
        }
        if dialog.pending.is_some() || (!cancel && !surfaces::escape(&response)) {
            self.dialog = Some(dialog);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_tokens_are_scoped_to_the_configured_host_and_port() -> Result<()> {
        let config = ConfigSet::new();
        let credentials = PushCredentials {
            token: Some(HttpsToken {
                host: "git.example.com:8443".into(),
                username: "tester".into(),
                token: "test-token".into(),
            }),
            helpers: HelperCredentialProvider::new(config.clone()),
        };
        let target = "https://git.example.com:8443/owner/project.git";
        assert_eq!(https_host(target).as_deref(), Some("git.example.com:8443"));
        let filled = credentials.fill(&Credential::from_url(target, &config).unwrap())?;
        assert_eq!(filled.username.as_deref(), Some("tester"));
        assert_eq!(filled.password.as_deref(), Some("test-token"));
        for other in [
            "http://git.example.com:8443/owner/project.git",
            "https://git.example.com/owner/project.git",
            "https://other.example.com:8443/owner/project.git",
        ] {
            assert!(
                credentials
                    .token_for(&Credential::from_url(other, &config).unwrap())
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn initializes_commits_pushes_and_fetches_with_a_custom_remote() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        Repository::init(workspace.path(), &Default::default())?;
        let folder = workspace.path().join("board");
        std::fs::create_dir(&folder)?;
        assert!(open(&folder)?.is_none());

        configure(&folder, &GitRemote::default())?;
        assert!(open(&folder)?.is_some());
        assert!(configured_remote(&folder)?.url.is_empty());
        let mut settings = GitRemote {
            name: "upstream".into(),
            url: "../remote.git".into(),
        };
        configure(&folder, &settings)?;
        assert_eq!(configured_remote(&folder)?, settings);
        settings.url = "../new-remote.git".into();
        configure(&folder, &settings)?;
        assert_eq!(configured_remote(&folder)?, settings);

        let repo = open(&folder)?.unwrap();
        assert!(has_changes(&repo)?);
        assert_eq!(
            std::fs::read_to_string(folder.join(".gitignore"))?,
            ".vspace.cache\n.DS_Store\n.history/\n*.lck\n"
        );
        repo.config().set_local("user.name", "Inventory Test")?;
        repo.config()
            .set_local("user.email", "inventory@example.invalid")?;
        std::fs::write(folder.join("board.kicad_pcb"), "first\nsecond\n")?;
        std::fs::write(folder.join("image.bin"), [0, 1, 2])?;
        assert!(has_changes(&repo)?);
        std::fs::write(folder.join(".DS_Store"), "ignored")?;
        std::fs::write(folder.join(".vspace.cache"), "ignored")?;
        std::fs::create_dir(folder.join(".history"))?;
        std::fs::write(folder.join(".history/board.kicad_pcb"), "ignored")?;
        std::fs::write(folder.join("board.kicad_pcb.lck"), "ignored")?;
        let reviewed = changes(&folder)?;
        assert_eq!(reviewed.len(), 3);
        assert_eq!(reviewed[0].path, ".gitignore");
        assert_eq!(reviewed[1].kind, "Added");
        assert_eq!(reviewed[1].lines, Some((2, 0)));
        assert_eq!(reviewed[2].lines, None);

        commit_with_scope(
            &repo,
            &CommitRequest::new("Initial board"),
            CommitScope::AutoStage,
            &mut NullProgress,
            &CancelToken::new(),
        )?;
        assert!(!has_changes(&repo)?);
        assert!(changes(&folder)?.is_empty());
        let repo = open(&folder)?.unwrap();
        assert!(has_unpushed_commits(&repo, &settings)?);
        let remote = workspace.path().join("new-remote.git");
        let mut options = InitOptions::default();
        options.bare = true;
        let (remote, _) = Repository::init(&remote, &options)?;
        push(&folder, &settings)?;
        let repo = open(&folder)?.unwrap();
        let branch = current_branch(&repo)?.unwrap();
        let branch_ref = RefNameRef::from_trusted(&branch);
        assert_eq!(
            remote.references().resolve(branch_ref)?,
            repo.references().resolve(branch_ref)?
        );
        assert!(!has_unpushed_commits(&repo, &settings)?);

        let peer_path = workspace.path().join("peer");
        let clone_settings = GitRemote {
            name: settings.name,
            url: folder.to_string_lossy().into_owned(),
        };
        fetch(&peer_path, &clone_settings)?;
        assert_eq!(configured_remote(&peer_path)?, clone_settings);
        assert_eq!(
            std::fs::read_to_string(peer_path.join("board.kicad_pcb"))?,
            "first\nsecond\n"
        );

        std::fs::write(folder.join("board.kicad_pcb"), "first\nthird\n")?;
        std::fs::remove_file(folder.join("image.bin"))?;
        let reviewed = changes(&folder)?;
        assert_eq!(reviewed.len(), 2);
        assert_eq!(reviewed[0].kind, "Modified");
        assert_eq!(reviewed[0].lines, Some((1, 1)));
        assert_eq!(reviewed[1].kind, "Removed");
        assert_eq!(reviewed[1].lines, None);
        Ok(())
    }
}
