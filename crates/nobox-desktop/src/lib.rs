//! Bounded, display-server-independent XDG desktop application discovery.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const MAX_DESKTOP_FILES: usize = 4_096;
const MAX_DESKTOP_FILE_BYTES: u64 = 262_144;
const MAX_DIRECTORY_DEPTH: usize = 8;
const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 16_384;

/// One conventional top-level application category.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ApplicationCategory {
    /// Utilities and accessories.
    Accessories,
    /// Software development tools.
    Development,
    /// Educational applications.
    Education,
    /// Games.
    Games,
    /// Graphics applications.
    Graphics,
    /// Network and Internet applications.
    Internet,
    /// Audio and video applications.
    Multimedia,
    /// Office applications.
    Office,
    /// Scientific applications.
    Science,
    /// Settings and system administration.
    System,
    /// Applications without a recognized main category.
    Other,
}

impl ApplicationCategory {
    /// Returns the stable user-visible category title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Accessories => "Accessories",
            Self::Development => "Development",
            Self::Education => "Education",
            Self::Games => "Games",
            Self::Graphics => "Graphics",
            Self::Internet => "Internet",
            Self::Multimedia => "Multimedia",
            Self::Office => "Office",
            Self::Science => "Science",
            Self::System => "System",
            Self::Other => "Other",
        }
    }
}

/// A direct process invocation parsed from a desktop entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchCommand {
    argv: Vec<String>,
    terminal: bool,
    working_directory: Option<PathBuf>,
}

impl LaunchCommand {
    /// Returns the executable followed by its arguments.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Returns whether the application should be wrapped in a terminal.
    #[must_use]
    pub const fn requires_terminal(&self) -> bool {
        self.terminal
    }

    /// Returns the requested working directory, when present.
    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }
}

/// One visible application from an XDG desktop entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopApplication {
    /// Desktop-file identifier after XDG precedence and de-duplication.
    pub desktop_id: String,
    /// Localized user-visible name.
    pub name: String,
    /// Optional icon name or path.
    pub icon: Option<String>,
    /// Conventional category selected from the entry's category list.
    pub category: ApplicationCategory,
    /// Direct process invocation.
    pub command: LaunchCommand,
    /// Whether the application requests startup notification.
    pub startup_notify: bool,
    /// Expected application window class, when declared.
    pub startup_wm_class: Option<String>,
}

/// Applications grouped into one conventional category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationGroup {
    /// Category represented by this group.
    pub category: ApplicationCategory,
    /// Applications ordered by localized name and stable desktop identifier.
    pub applications: Vec<DesktopApplication>,
}

/// Complete visible application catalog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplicationCatalog {
    groups: Vec<ApplicationGroup>,
    skipped_files: usize,
}

impl ApplicationCatalog {
    /// Discovers applications through the current XDG environment.
    #[must_use]
    pub fn discover() -> Self {
        let directories = data_directories();
        let locales = locale_candidates();
        let desktops = current_desktops();
        let executable_paths = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        Self::from_directories(&directories, &locales, &desktops, &executable_paths)
    }

    /// Builds a catalog from explicit application directories.
    ///
    /// Directories are ordered from highest to lowest precedence. Each path is
    /// the directory containing desktop files, not its parent data directory.
    #[must_use]
    pub fn from_directories(
        directories: &[PathBuf],
        locales: &[String],
        desktops: &[String],
        executable_paths: &[PathBuf],
    ) -> Self {
        let mut seen = BTreeSet::new();
        let mut applications = Vec::new();
        let mut skipped_files = 0_usize;
        let mut remaining = MAX_DESKTOP_FILES;
        let mut source = String::new();

        for directory in directories {
            if remaining == 0 {
                break;
            }
            let files = desktop_files(directory, remaining);
            remaining = remaining.saturating_sub(files.len());
            for path in files {
                let Some(desktop_id) = desktop_id(directory, &path) else {
                    skipped_files = skipped_files.saturating_add(1);
                    continue;
                };
                if !seen.insert(desktop_id.clone()) {
                    continue;
                }
                match parse_application(
                    &path,
                    desktop_id,
                    locales,
                    desktops,
                    executable_paths,
                    &mut source,
                ) {
                    Some(application) => applications.push(application),
                    None => skipped_files = skipped_files.saturating_add(1),
                }
            }
        }

        applications.sort_by_cached_key(|application| {
            (
                application.category,
                application.name.to_lowercase(),
                application.name.clone(),
                application.desktop_id.clone(),
            )
        });
        let mut grouped = BTreeMap::<ApplicationCategory, Vec<DesktopApplication>>::new();
        for application in applications {
            grouped
                .entry(application.category)
                .or_default()
                .push(application);
        }
        let groups = grouped
            .into_iter()
            .map(|(category, applications)| ApplicationGroup {
                category,
                applications,
            })
            .collect();
        Self {
            groups,
            skipped_files,
        }
    }

    /// Returns ordered, non-empty application groups.
    #[must_use]
    pub fn groups(&self) -> &[ApplicationGroup] {
        &self.groups
    }

    /// Returns the number of hidden, unavailable, or invalid files omitted.
    #[must_use]
    pub const fn skipped_files(&self) -> usize {
        self.skipped_files
    }

    /// Returns the number of visible applications.
    #[must_use]
    pub fn application_count(&self) -> usize {
        self.groups
            .iter()
            .map(|group| group.applications.len())
            .sum()
    }
}

fn data_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(data_home) = data_home.filter(|path| !path.as_os_str().is_empty()) {
        directories.push(data_home.join("applications"));
    }
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories.extend(env::split_paths(&data_dirs).map(|path| path.join("applications")));
    directories
}

fn current_desktops() -> Vec<String> {
    let mut desktops = env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(':')
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if !desktops
        .iter()
        .any(|desktop| desktop.eq_ignore_ascii_case("nobox"))
    {
        desktops.push("nobox".to_owned());
    }
    desktops
}

fn locale_candidates() -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()));
    let Some(locale) = locale else {
        return Vec::new();
    };
    let (base, modifier) = locale
        .split_once('@')
        .map_or((locale.as_str(), None), |(base, modifier)| {
            (base, Some(modifier))
        });
    let base = base.split('.').next().unwrap_or(base);
    let language = base.split('_').next().unwrap_or(base);
    let mut candidates = Vec::with_capacity(4);
    if let Some(modifier) = modifier {
        candidates.push(format!("{base}@{modifier}"));
    }
    candidates.push(base.to_owned());
    if let Some(modifier) = modifier {
        candidates.push(format!("{language}@{modifier}"));
    }
    candidates.push(language.to_owned());
    candidates.dedup();
    candidates
}

fn desktop_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        if files.len() >= limit {
            break;
        }
        if depth > MAX_DIRECTORY_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_cached_key(std::fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            if files.len() >= limit {
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                pending.push((entry.path(), depth.saturating_add(1)));
            } else if file_type.is_file() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "desktop")
                {
                    files.push(path);
                }
            }
        }
    }
    files.sort_unstable();
    files.truncate(limit);
    files
}

fn desktop_id(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut id = String::new();
    for component in relative.components() {
        let part = component.as_os_str().to_str()?;
        if !id.is_empty() {
            id.push('-');
        }
        id.push_str(part);
    }
    Some(id)
}

fn parse_application(
    path: &Path,
    desktop_id: String,
    locales: &[String],
    desktops: &[String],
    executable_paths: &[PathBuf],
    source: &mut String,
) -> Option<DesktopApplication> {
    let mut file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_DESKTOP_FILE_BYTES || !metadata.is_file() {
        return None;
    }
    source.clear();
    std::io::Read::read_to_string(&mut file, source).ok()?;
    let values = desktop_entry_values(source)?;
    if values.get("Type").copied() != Some("Application")
        || desktop_bool(values.get("Hidden").copied())
        || desktop_bool(values.get("NoDisplay").copied())
        || !desktop_visible(&values, desktops)
    {
        return None;
    }
    if let Some(try_exec) = values.get("TryExec")
        && !executable_exists(try_exec, executable_paths)
    {
        return None;
    }
    let name = localized_value(&values, "Name", locales)?;
    if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return None;
    }
    let icon = values
        .get("Icon")
        .and_then(|value| unescape_string(value))
        .filter(|value| !value.is_empty() && value.len() <= 4_096);
    let argv = parse_exec(values.get("Exec")?, &name, icon.as_deref(), path)?;
    let categories = values
        .get("Categories")
        .map_or_else(Vec::new, |value| desktop_list(value));
    let working_directory = values
        .get("Path")
        .and_then(|value| unescape_string(value))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let startup_wm_class = values
        .get("StartupWMClass")
        .and_then(|value| unescape_string(value))
        .filter(|value| !value.is_empty() && value.len() <= 255);
    Some(DesktopApplication {
        desktop_id,
        name,
        icon,
        category: application_category(&categories),
        command: LaunchCommand {
            argv,
            terminal: desktop_bool(values.get("Terminal").copied()),
            working_directory,
        },
        startup_notify: desktop_bool(values.get("StartupNotify").copied()),
        startup_wm_class,
    })
}

fn desktop_entry_values(source: &str) -> Option<BTreeMap<&str, &str>> {
    let mut values = BTreeMap::new();
    let mut in_desktop_entry = false;
    for line in source.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.is_empty() || key.len() > 255 || value.len() > MAX_ARGUMENT_BYTES {
            return None;
        }
        values.entry(key).or_insert(value);
    }
    Some(values)
}

fn localized_value(values: &BTreeMap<&str, &str>, key: &str, locales: &[String]) -> Option<String> {
    let mut lookup = String::new();
    locales
        .iter()
        .find_map(|locale| {
            lookup.clear();
            lookup.push_str(key);
            lookup.push('[');
            lookup.push_str(locale);
            lookup.push(']');
            values.get(lookup.as_str()).copied()
        })
        .or_else(|| values.get(key).copied())
        .and_then(unescape_string)
}

fn desktop_bool(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn desktop_visible(values: &BTreeMap<&str, &str>, desktops: &[String]) -> bool {
    if let Some(only) = values.get("OnlyShowIn") {
        let mut listed = false;
        let mut allowed = false;
        for item in only.split(';').filter(|item| !item.is_empty()) {
            listed = true;
            if desktops
                .iter()
                .any(|desktop| desktop.eq_ignore_ascii_case(item))
            {
                allowed = true;
                break;
            }
        }
        if listed && !allowed {
            return false;
        }
    }
    values.get("NotShowIn").is_none_or(|excluded| {
        !excluded
            .split(';')
            .filter(|item| !item.is_empty())
            .any(|blocked| {
                desktops
                    .iter()
                    .any(|desktop| desktop.eq_ignore_ascii_case(blocked))
            })
    })
}

fn desktop_list(value: &str) -> Vec<&str> {
    value.split(';').filter(|item| !item.is_empty()).collect()
}

fn application_category(categories: &[&str]) -> ApplicationCategory {
    for (name, category) in [
        ("Utility", ApplicationCategory::Accessories),
        ("Development", ApplicationCategory::Development),
        ("Education", ApplicationCategory::Education),
        ("Game", ApplicationCategory::Games),
        ("Graphics", ApplicationCategory::Graphics),
        ("Network", ApplicationCategory::Internet),
        ("AudioVideo", ApplicationCategory::Multimedia),
        ("Audio", ApplicationCategory::Multimedia),
        ("Video", ApplicationCategory::Multimedia),
        ("Office", ApplicationCategory::Office),
        ("Science", ApplicationCategory::Science),
        ("Settings", ApplicationCategory::System),
        ("System", ApplicationCategory::System),
    ] {
        if categories.contains(&name) {
            return category;
        }
    }
    ApplicationCategory::Other
}

fn executable_exists(value: &str, executable_paths: &[PathBuf]) -> bool {
    let path = Path::new(value);
    if path.components().count() > 1 {
        return is_executable(path);
    }
    executable_paths
        .iter()
        .any(|directory| is_executable(&directory.join(path)))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn unescape_string(value: &str) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        output.push(match characters.next()? {
            's' => ' ',
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            _ => return None,
        });
    }
    Some(output)
}

fn parse_exec(value: &str, name: &str, icon: Option<&str>, path: &Path) -> Option<Vec<String>> {
    let tokens = tokenize_exec(value)?;
    let mut argv = Vec::with_capacity(tokens.len().saturating_add(1));
    for token in tokens {
        if token == "%i" {
            if let Some(icon) = icon {
                argv.push("--icon".to_owned());
                argv.push(icon.to_owned());
            }
            continue;
        }
        let mut expanded = String::with_capacity(token.len());
        let mut characters = token.chars();
        while let Some(character) = characters.next() {
            if character != '%' {
                expanded.push(character);
                continue;
            }
            match characters.next()? {
                '%' => expanded.push('%'),
                'c' => expanded.push_str(name),
                'k' => expanded.push_str(path.to_str()?),
                'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
                'i' => return None,
                _ => return None,
            }
        }
        if !expanded.is_empty() {
            argv.push(expanded);
        }
        if argv.len() > MAX_ARGUMENTS
            || argv.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
        {
            return None;
        }
    }
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

fn tokenize_exec(value: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            token.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if escaped || quoted {
        return None;
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "nobox-desktop-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create fixture");
            Self { path }
        }

        fn write(&self, relative: &str, source: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create fixture parent");
            }
            fs::write(path, source).expect("write desktop entry");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("remove fixture");
        }
    }

    fn catalog(directories: &[&Fixture]) -> ApplicationCatalog {
        ApplicationCatalog::from_directories(
            &directories
                .iter()
                .map(|fixture| fixture.path.clone())
                .collect::<Vec<_>>(),
            &["nb_NO".to_owned(), "nb".to_owned()],
            &["nobox".to_owned()],
            &[],
        )
    }

    #[test]
    fn localized_entries_are_categorized_sorted_and_expanded_without_a_shell() {
        let fixture = Fixture::new();
        fixture.write(
            "z.desktop",
            "[Desktop Entry]\nType=Application\nName=Zulu\nExec=viewer %F\nCategories=Graphics;\n",
        );
        fixture.write(
            "a.desktop",
            "[Desktop Entry]\nType=Application\nName=Alpha\nName[nb]=Alfa\nIcon=alpha\nExec=alpha --name %c %i %%\nCategories=Utility;\nStartupNotify=true\nStartupWMClass=Alpha\n",
        );

        let catalog = catalog(&[&fixture]);

        assert_eq!(catalog.application_count(), 2);
        assert_eq!(
            catalog.groups()[0].category,
            ApplicationCategory::Accessories
        );
        let application = &catalog.groups()[0].applications[0];
        assert_eq!(application.name, "Alfa");
        assert_eq!(
            application.command.argv(),
            ["alpha", "--name", "Alfa", "--icon", "alpha", "%"]
        );
        assert!(application.startup_notify);
        assert_eq!(application.startup_wm_class.as_deref(), Some("Alpha"));
        assert_eq!(catalog.groups()[1].category, ApplicationCategory::Graphics);
    }

    #[test]
    fn user_hidden_entry_shadows_a_system_application() {
        let user = Fixture::new();
        let system = Fixture::new();
        user.write(
            "nested/tool.desktop",
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=hidden\nHidden=true\n",
        );
        system.write(
            "nested/tool.desktop",
            "[Desktop Entry]\nType=Application\nName=Visible\nExec=visible\n",
        );

        let catalog = catalog(&[&user, &system]);

        assert_eq!(catalog.application_count(), 0);
        assert_eq!(catalog.skipped_files(), 1);
    }

    #[test]
    fn visibility_and_try_exec_are_enforced() {
        let fixture = Fixture::new();
        fixture.write(
            "allowed.desktop",
            "[Desktop Entry]\nType=Application\nName=Allowed\nExec=allowed\nOnlyShowIn=nobox;\n",
        );
        fixture.write(
            "blocked.desktop",
            "[Desktop Entry]\nType=Application\nName=Blocked\nExec=blocked\nNotShowIn=nobox;\n",
        );
        fixture.write(
            "missing.desktop",
            "[Desktop Entry]\nType=Application\nName=Missing\nExec=missing\nTryExec=definitely-not-installed\n",
        );

        let catalog = catalog(&[&fixture]);

        assert_eq!(catalog.application_count(), 1);
        assert_eq!(catalog.groups()[0].applications[0].name, "Allowed");
    }

    #[test]
    fn malformed_or_hostile_exec_values_are_rejected() {
        let fixture = Fixture::new();
        fixture.write(
            "quote.desktop",
            "[Desktop Entry]\nType=Application\nName=Quote\nExec=tool \"unterminated\n",
        );
        fixture.write(
            "field.desktop",
            "[Desktop Entry]\nType=Application\nName=Field\nExec=tool %Z\n",
        );

        let catalog = catalog(&[&fixture]);

        assert_eq!(catalog.application_count(), 0);
        assert_eq!(catalog.skipped_files(), 2);
    }
}
