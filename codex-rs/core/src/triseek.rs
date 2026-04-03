use anyhow::Context;
use once_cell::sync::Lazy;
use search_core::{
    AdaptiveRoute, CaseMode, QueryRequest, RepoCategory, SearchEngineKind, SearchHit, SearchKind,
    plan_query, route_query,
};
use search_index::{
    BuildConfig, SearchEngine, build_index, index_exists, measure_repository, read_index_metadata,
    update_index,
};
use codex_git_utils::get_git_repo_root;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::task;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::config::TriSeekRepoCategory;

const ENABLE_ENV: &str = "CODEX_TRISEEK";
const AUTO_BUILD_ENV: &str = "CODEX_TRISEEK_AUTO_BUILD";
const LOG_ROUTES_ENV: &str = "CODEX_TRISEEK_LOG_ROUTES";
const MIN_CATEGORY_ENV: &str = "CODEX_TRISEEK_MIN_CATEGORY";
const STATUS_FILE: &str = "status.json";
const LOCK_FILE: &str = "build.lock";

static ACTIVE_BUILDS: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

#[derive(Clone)]
struct TriSeekSettings {
    enabled: bool,
    auto_build: bool,
    log_routes: bool,
    min_index_category: RepoCategory,
    index_root: PathBuf,
}

pub enum SearchOutcome {
    UsedIndexed(Vec<String>),
    FallbackToRipgrep,
}

#[derive(Clone)]
struct RepoContext {
    repo_root: PathBuf,
    scope: SearchScope,
}

#[derive(Clone)]
enum SearchScope {
    WholeRepo,
    Directory(String),
    File(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StatusState {
    Pending,
    Ready,
    SkippedSmallRepo,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexStatus {
    state: StatusState,
    repo_root: String,
    repo_category: Option<RepoCategory>,
    commit_sha: Option<String>,
    updated_at: String,
    message: Option<String>,
}

enum IndexAvailability {
    Missing,
    Building,
    SkippedSmallRepo,
    Ready(search_core::IndexMetadata),
    Stale,
}

pub async fn maybe_search_matching_files(
    config: &Config,
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
) -> anyhow::Result<SearchOutcome> {
    let settings = settings_from_config(config);
    if !settings.enabled {
        return Ok(SearchOutcome::FallbackToRipgrep);
    }

    let context = match resolve_repo_context(search_path) {
        Ok(context) => context,
        Err(err) => {
            warn!("triseek repo resolution failed, falling back to ripgrep: {err:#}");
            return Ok(SearchOutcome::FallbackToRipgrep);
        }
    };

    let index_dir = index_dir_for_repo(&context.repo_root, &settings.index_root)?;
    match inspect_index_state(&context.repo_root, &index_dir) {
        IndexAvailability::Ready(metadata) => {
            let request = build_request(pattern, include, &context.scope);
            let plan = plan_query(&request);
            let routing = route_query(&request, Some(&metadata.repo_stats), &plan, true, false);
            if settings.log_routes {
                info!(
                    repo_root = %context.repo_root.display(),
                    selected_engine = ?routing.selected_engine,
                    reason = %routing.reason,
                    "triseek route decision"
                );
            }

            if !matches!(routing.selected_engine, AdaptiveRoute::Indexed) {
                return Ok(SearchOutcome::FallbackToRipgrep);
            }

            let repo_root = context.repo_root.clone();
            let index_dir = index_dir.clone();
            let request_clone = request.clone();
            let hits = task::spawn_blocking(move || {
                indexed_paths_for_request(&repo_root, &index_dir, &request_clone, limit)
            })
            .await
            .context("waiting for indexed search task failed")??;
            Ok(SearchOutcome::UsedIndexed(hits))
        }
        IndexAvailability::Missing => {
            maybe_enqueue_background_build(
                context.repo_root.clone(),
                index_dir.clone(),
                settings.clone(),
            );
            Ok(SearchOutcome::FallbackToRipgrep)
        }
        IndexAvailability::Building | IndexAvailability::SkippedSmallRepo => {
            Ok(SearchOutcome::FallbackToRipgrep)
        }
        IndexAvailability::Stale => {
            maybe_enqueue_background_build(context.repo_root.clone(), index_dir.clone(), settings);
            Ok(SearchOutcome::FallbackToRipgrep)
        }
    }
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
    })
}

fn settings_from_config(config: &Config) -> TriSeekSettings {
    TriSeekSettings {
        enabled: env_flag(ENABLE_ENV).unwrap_or(config.triseek.enabled),
        auto_build: env_flag(AUTO_BUILD_ENV).unwrap_or(config.triseek.auto_build),
        log_routes: env_flag(LOG_ROUTES_ENV).unwrap_or(config.triseek.log_routes),
        min_index_category: std::env::var(MIN_CATEGORY_ENV)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
            .map(parse_repo_category_override)
            .unwrap_or_else(|| tri_category_to_repo_category(config.triseek.min_index_category)),
        index_root: config.triseek.index_root.clone(),
    }
}

fn parse_repo_category_override(value: &str) -> RepoCategory {
    match value {
        "small" => RepoCategory::Small,
        "large" => RepoCategory::Large,
        "very_large" => RepoCategory::VeryLarge,
        _ => RepoCategory::Medium,
    }
}

fn tri_category_to_repo_category(category: TriSeekRepoCategory) -> RepoCategory {
    match category {
        TriSeekRepoCategory::Small => RepoCategory::Small,
        TriSeekRepoCategory::Medium => RepoCategory::Medium,
        TriSeekRepoCategory::Large => RepoCategory::Large,
        TriSeekRepoCategory::VeryLarge => RepoCategory::VeryLarge,
    }
}

fn resolve_repo_context(search_path: &Path) -> anyhow::Result<RepoContext> {
    let canonical_search_path = dunce::canonicalize(search_path)
        .with_context(|| format!("failed to canonicalize {}", search_path.display()))?;
    let search_dir = if canonical_search_path.is_dir() {
        canonical_search_path.clone()
    } else {
        canonical_search_path
            .parent()
            .map(Path::to_path_buf)
            .context("search path does not have a parent directory")?
    };

    let repo_root = get_git_repo_root(&search_dir)
        .and_then(|path| dunce::canonicalize(path).ok())
        .unwrap_or(search_dir.clone());
    let scope = if canonical_search_path == repo_root {
        SearchScope::WholeRepo
    } else if canonical_search_path.is_dir() {
        let mut prefix = normalize_relative_path(
            canonical_search_path
                .strip_prefix(&repo_root)
                .context("search directory is outside repo root")?,
        );
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        SearchScope::Directory(prefix)
    } else {
        SearchScope::File(normalize_relative_path(
            canonical_search_path
                .strip_prefix(&repo_root)
                .context("search file is outside repo root")?,
        ))
    };

    Ok(RepoContext { repo_root, scope })
}

fn index_dir_for_repo(repo_root: &Path, index_root: &Path) -> anyhow::Result<PathBuf> {
    let canonical_root = dunce::canonicalize(repo_root)
        .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;
    let mut hasher = Sha1::new();
    hasher.update(canonical_root.as_os_str().as_encoded_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Ok(index_root.join(digest))
}

fn inspect_index_state(repo_root: &Path, index_dir: &Path) -> IndexAvailability {
    let metadata = match index_exists(index_dir) {
        true => match read_index_metadata(index_dir) {
            Ok(metadata) => metadata,
            Err(err) => {
                warn!(
                    "triseek metadata read failed for {}: {err:#}",
                    index_dir.display()
                );
                return IndexAvailability::Missing;
            }
        },
        false => {
            if lock_path(index_dir).exists() {
                return IndexAvailability::Building;
            }
            return match read_status(index_dir) {
                Some(status) if status.state == StatusState::SkippedSmallRepo => {
                    IndexAvailability::SkippedSmallRepo
                }
                _ => IndexAvailability::Missing,
            };
        }
    };

    if metadata.repo_stats.repo_root != repo_root.display().to_string() {
        return IndexAvailability::Stale;
    }

    if let Some(current_head) = current_git_head(repo_root) {
        let indexed_head = metadata.repo_stats.commit_sha.trim();
        if !indexed_head.is_empty() && indexed_head != "unresolved" && indexed_head != current_head
        {
            return IndexAvailability::Stale;
        }
    }

    IndexAvailability::Ready(metadata)
}

fn maybe_enqueue_background_build(
    repo_root: PathBuf,
    index_dir: PathBuf,
    settings: TriSeekSettings,
) {
    if !settings.auto_build {
        return;
    }
    if lock_path(&index_dir).exists() {
        return;
    }

    let mut active = match ACTIVE_BUILDS.lock() {
        Ok(active) => active,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !active.insert(index_dir.clone()) {
        return;
    }
    drop(active);

    tokio::spawn(async move {
        let repo_root_for_task = repo_root.clone();
        let index_dir_for_task = index_dir.clone();
        let result = task::spawn_blocking(move || {
            build_or_update_index(&repo_root_for_task, &index_dir_for_task, &settings)
        })
        .await;

        match result {
            Ok(Ok(())) => {
                debug!(
                    "triseek background build finished for {}",
                    repo_root.display()
                );
            }
            Ok(Err(err)) => {
                warn!(
                    "triseek background build failed for {}: {err:#}",
                    repo_root.display()
                );
            }
            Err(err) => {
                warn!(
                    "triseek background build task join failed for {}: {err:#}",
                    repo_root.display()
                );
            }
        }

        let mut active = match ACTIVE_BUILDS.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.remove(&index_dir);
    });
}

fn build_or_update_index(
    repo_root: &Path,
    index_dir: &Path,
    settings: &TriSeekSettings,
) -> anyhow::Result<()> {
    let Some(_guard) = BuildLockGuard::acquire(index_dir)? else {
        return Ok(());
    };

    write_status(
        index_dir,
        &IndexStatus {
            state: StatusState::Pending,
            repo_root: repo_root.display().to_string(),
            repo_category: None,
            commit_sha: current_git_head(repo_root),
            updated_at: timestamp_now(),
            message: Some("building index".to_string()),
        },
    )?;

    let build_config = BuildConfig::default();
    let stats = measure_repository(repo_root, &build_config)
        .with_context(|| format!("failed to measure repository {}", repo_root.display()))?;
    let category = stats.category.unwrap_or(RepoCategory::Small);
    if !should_index_category(category, settings.min_index_category) {
        write_status(
            index_dir,
            &IndexStatus {
                state: StatusState::SkippedSmallRepo,
                repo_root: repo_root.display().to_string(),
                repo_category: Some(category),
                commit_sha: current_git_head(repo_root),
                updated_at: timestamp_now(),
                message: Some(format!("skipped index build for {category:?} repo")),
            },
        )?;
        return Ok(());
    }

    std::fs::create_dir_all(index_dir)?;
    if index_exists(index_dir) {
        update_index(repo_root, index_dir, &build_config)
            .with_context(|| format!("failed to update index at {}", index_dir.display()))?;
    } else {
        build_index(repo_root, index_dir, &build_config)
            .with_context(|| format!("failed to build index at {}", index_dir.display()))?;
    }
    let metadata = read_index_metadata(index_dir).with_context(|| {
        format!(
            "failed to read completed index metadata at {}",
            index_dir.display()
        )
    })?;
    write_status(
        index_dir,
        &IndexStatus {
            state: StatusState::Ready,
            repo_root: repo_root.display().to_string(),
            repo_category: metadata.repo_stats.category,
            commit_sha: Some(metadata.repo_stats.commit_sha.clone()),
            updated_at: timestamp_now(),
            message: Some("index ready".to_string()),
        },
    )?;
    info!(
        repo_root = %repo_root.display(),
        index_dir = %index_dir.display(),
        repo_category = ?metadata.repo_stats.category,
        "triseek index ready"
    );
    Ok(())
}

fn indexed_paths_for_request(
    repo_root: &Path,
    index_dir: &Path,
    request: &QueryRequest,
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let engine = SearchEngine::open(index_dir)
        .with_context(|| format!("failed to open index {}", index_dir.display()))?;
    let execution = engine.search(request).context("indexed search failed")?;
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    for hit in execution.hits {
        let relative = match hit {
            SearchHit::Content { path, .. } => path,
            SearchHit::Path { path } => path,
        };
        let absolute = repo_root.join(&relative);
        let rendered = absolute.display().to_string();
        if seen.insert(rendered.clone()) {
            paths.push(rendered);
        }
    }

    paths.sort_by(|left, right| {
        modified_time_key(right)
            .cmp(&modified_time_key(left))
            .then(left.cmp(right))
    });
    paths.truncate(limit);
    Ok(paths)
}

fn build_request(pattern: &str, include: Option<&str>, scope: &SearchScope) -> QueryRequest {
    let mut request = QueryRequest {
        kind: SearchKind::Regex,
        engine: SearchEngineKind::Auto,
        pattern: pattern.to_string(),
        case_mode: CaseMode::Sensitive,
        globs: include
            .map(|glob| vec![glob.to_string()])
            .unwrap_or_default(),
        // `grep_files` limits by unique files, not by line hits. Let the
        // engine collect all matching hits, then dedupe and truncate by file
        // path in `indexed_paths_for_request`.
        max_results: None,
        ..Default::default()
    };

    match scope {
        SearchScope::WholeRepo => {}
        SearchScope::Directory(prefix) => request.path_prefixes.push(prefix.clone()),
        SearchScope::File(relative_path) => request.exact_paths.push(relative_path.clone()),
    }

    request
}

fn modified_time_key(path: &str) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn current_git_head(repo_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8(output.stdout).ok()?;
    let trimmed = head.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn should_index_category(category: RepoCategory, min_index_category: RepoCategory) -> bool {
    category_rank(category) >= category_rank(min_index_category)
}

fn category_rank(category: RepoCategory) -> u8 {
    match category {
        RepoCategory::Small => 0,
        RepoCategory::Medium => 1,
        RepoCategory::Large => 2,
        RepoCategory::VeryLarge => 3,
    }
}

fn status_path(index_dir: &Path) -> PathBuf {
    index_dir.join(STATUS_FILE)
}

fn lock_path(index_dir: &Path) -> PathBuf {
    index_dir.join(LOCK_FILE)
}

fn write_status(index_dir: &Path, status: &IndexStatus) -> anyhow::Result<()> {
    std::fs::create_dir_all(index_dir)?;
    std::fs::write(status_path(index_dir), serde_json::to_vec_pretty(status)?)
        .with_context(|| format!("failed to write status under {}", index_dir.display()))?;
    Ok(())
}

fn read_status(index_dir: &Path) -> Option<IndexStatus> {
    let bytes = std::fs::read(status_path(index_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn timestamp_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

struct BuildLockGuard {
    path: PathBuf,
}

impl BuildLockGuard {
    fn acquire(index_dir: &Path) -> anyhow::Result<Option<Self>> {
        std::fs::create_dir_all(index_dir)?;
        let path = lock_path(index_dir);
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(_) => Ok(Some(Self { path })),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

impl Drop for BuildLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn builds_directory_scoped_request() {
        let request = build_request(
            "needle",
            Some("*.rs"),
            &SearchScope::Directory("src/".to_string()),
        );
        assert_eq!(request.pattern, "needle");
        assert_eq!(request.globs, vec!["*.rs".to_string()]);
        assert_eq!(request.path_prefixes, vec!["src/".to_string()]);
        assert_eq!(request.max_results, None);
    }

    #[test]
    fn normalizes_relative_path_with_forward_slashes() {
        let path = PathBuf::from("src").join("tools").join("grep.rs");
        assert_eq!(normalize_relative_path(&path), "src/tools/grep.rs");
    }

    #[tokio::test]
    #[serial]
    async fn enabled_search_falls_back_for_small_repo() {
        let token = "tri_seek_unique_symbol_42";
        let codex_home = TempDir::new().expect("codex home");
        let repo = TempDir::new().expect("repo");
        std::fs::create_dir_all(repo.path().join(".git")).expect("git dir");
        std::fs::write(
            repo.path().join("match.rs"),
            format!("fn example() {{ println!(\"{token}\"); }}\n"),
        )
        .expect("match file");
        std::fs::write(repo.path().join("other.txt"), "no match here\n").expect("other file");

        let mut config = build_test_config(codex_home.path());
        config.triseek.enabled = true;
        config.triseek.min_index_category = TriSeekRepoCategory::Small;

        let index_dir =
            index_dir_for_repo(repo.path(), &config.triseek.index_root).expect("index dir");
        build_or_update_index(repo.path(), &index_dir, &settings_from_config(&config))
            .expect("build index");

        let outcome = maybe_search_matching_files(&config, token, Some("*.rs"), repo.path(), 10)
            .await
            .expect("search outcome");
        assert!(matches!(outcome, SearchOutcome::FallbackToRipgrep));
    }

    #[test]
    fn indexed_paths_helper_reads_ready_index() {
        let token = "tri_seek_unique_symbol_42";
        let codex_home = TempDir::new().expect("codex home");
        let repo = TempDir::new().expect("repo");
        std::fs::create_dir_all(repo.path().join(".git")).expect("git dir");
        std::fs::write(
            repo.path().join("match.rs"),
            format!("fn example() {{ println!(\"{token}\"); }}\n"),
        )
        .expect("match file");
        std::fs::write(repo.path().join("other.txt"), "no match here\n").expect("other file");

        let mut config = build_test_config(codex_home.path());
        config.triseek.min_index_category = TriSeekRepoCategory::Small;

        let index_dir =
            index_dir_for_repo(repo.path(), &config.triseek.index_root).expect("index dir");
        build_or_update_index(repo.path(), &index_dir, &settings_from_config(&config))
            .expect("build index");

        let request = build_request(token, Some("*.rs"), &SearchScope::WholeRepo);
        let paths = indexed_paths_for_request(repo.path(), &index_dir, &request, 10)
            .expect("indexed paths");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("match.rs"));
    }

    #[test]
    fn indexed_paths_limit_is_applied_after_file_dedup() {
        let token = "tri_seek_limit_test";
        let codex_home = TempDir::new().expect("codex home");
        let repo = TempDir::new().expect("repo");
        std::fs::create_dir_all(repo.path().join(".git")).expect("git dir");
        std::fs::write(
            repo.path().join("alpha.rs"),
            format!("{token}\n{token}\n{token}\n"),
        )
        .expect("alpha");
        std::fs::write(repo.path().join("beta.rs"), format!("{token}\n")).expect("beta");
        std::fs::write(repo.path().join("gamma.rs"), format!("{token}\n")).expect("gamma");

        let mut config = build_test_config(codex_home.path());
        config.triseek.min_index_category = TriSeekRepoCategory::Small;

        let index_dir =
            index_dir_for_repo(repo.path(), &config.triseek.index_root).expect("index dir");
        build_or_update_index(repo.path(), &index_dir, &settings_from_config(&config))
            .expect("build index");

        let request = build_request(token, Some("*.rs"), &SearchScope::WholeRepo);
        let paths =
            indexed_paths_for_request(repo.path(), &index_dir, &request, 3).expect("indexed paths");
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn category_rank_orders_repo_sizes() {
        assert!(category_rank(RepoCategory::Medium) > category_rank(RepoCategory::Small));
        assert!(category_rank(RepoCategory::VeryLarge) > category_rank(RepoCategory::Large));
    }

    #[test]
    fn settings_env_override_wins() {
        let codex_home = TempDir::new().expect("codex home");
        let mut config = build_test_config(codex_home.path());
        config.triseek.enabled = false;
        config.triseek.min_index_category = TriSeekRepoCategory::Small;

        let _enabled = EnvVarGuard::set(ENABLE_ENV, "1");
        let _min_category = EnvVarGuard::set(MIN_CATEGORY_ENV, "large");
        let settings = settings_from_config(&config);
        assert!(settings.enabled);
        assert_eq!(settings.min_index_category, RepoCategory::Large);
    }

    #[test]
    fn index_status_round_trips() {
        let index_dir = TempDir::new().expect("index dir");
        let status = IndexStatus {
            state: StatusState::Pending,
            repo_root: "/tmp/repo".to_string(),
            repo_category: Some(RepoCategory::Medium),
            commit_sha: Some("abc123".to_string()),
            updated_at: timestamp_now(),
            message: Some("building".to_string()),
        };
        write_status(index_dir.path(), &status).expect("write status");
        let round_tripped = read_status(index_dir.path()).expect("status present");
        assert_eq!(round_tripped.state, StatusState::Pending);
        assert_eq!(round_tripped.repo_category, Some(RepoCategory::Medium));
        assert_eq!(round_tripped.commit_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn repo_category_filter_is_configurable() {
        assert!(should_index_category(
            RepoCategory::Small,
            RepoCategory::Small
        ));
        assert!(!should_index_category(
            RepoCategory::Large,
            RepoCategory::VeryLarge
        ));
        assert!(should_index_category(
            RepoCategory::VeryLarge,
            RepoCategory::VeryLarge
        ));
    }

    fn build_test_config(codex_home: &Path) -> config::Config {
        let mut config = config::test_config();
        config.codex_home = codex_home.to_path_buf();
        config.triseek.index_root = codex_home.join("triseek").join("indexes");
        config.triseek.auto_build = true;
        config.triseek.log_routes = false;
        config
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl Into<String>) -> Self {
            let previous = std::env::var(key).ok();
            // Safety: these tests are run serially and restore the previous value.
            unsafe {
                std::env::set_var(key, value.into());
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(previous) => {
                    // Safety: these tests are run serially and restore the previous value.
                    unsafe {
                        std::env::set_var(self.key, previous);
                    }
                }
                None => {
                    // Safety: these tests are run serially and restore the previous value.
                    unsafe {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
    }
}
