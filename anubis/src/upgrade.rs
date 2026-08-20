//! Reading and rewriting the framework dependency an application declares.
//!
//! An Anubis application depends on the framework twice, once per end: the
//! `anubis` crate in `backend/Cargo.toml` and the `@jalapenolabs/anubis`
//! package in `frontend/package.json`. The two ship as one release and carry
//! one version (see `docs/upgrading.md`), so upgrading is rewriting both
//! requirements and re-running the generators. This module is the pure half of
//! `anubis upgrade`: it reads a declaration out of a manifest, classifies what
//! kind of dependency it is, and renders the manifest again with the
//! requirement pointing at another version. The CLI owns the filesystem, the
//! registry call, and the subprocesses.
//!
//! Rewriting is deliberate string surgery over the manifest the application
//! owns, in the same spirit as [`crate::scaffold`]: the file comes back byte
//! for byte except for the requirement itself, so comments, ordering, and
//! formatting survive. A shape this module does not recognize is refused by
//! name rather than guessed at, because a manifest is the one file an
//! application cannot afford a generator to be creative with.
//!
//! ```
//! use anubis::upgrade::{Dependency, cargo_declaration};
//!
//! let manifest = "[dependencies]\nanubis = \"0.2.0\"\n";
//! let declaration = cargo_declaration(manifest).unwrap();
//! assert!(matches!(declaration.dependency(), Dependency::Published(_)));
//! assert_eq!(
//!     declaration.upgraded(&"0.3.0".parse().unwrap()).unwrap(),
//!     "[dependencies]\nanubis = \"0.3.0\"\n",
//! );
//! ```

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::ops::Range;

use semver::Version;

use crate::eject::PACKAGE;

/// The crate an application's `backend/Cargo.toml` depends on.
pub const CRATE: &str = "anubis";

/// The crates.io endpoint that answers what the latest `anubis` release is.
///
/// The API answers a JSON object whose `crate` member carries the version
/// fields, and a crate it has never heard of comes back `404` with an `errors`
/// member. [`latest_published`] reads both shapes.
pub const REGISTRY_ENDPOINT: &str = "https://crates.io/api/v1/crates/anubis";

/// Where the release workflow puts a version's notes, one tag per release.
const RELEASES: &str = "https://github.com/JalapenoLabs/Anubis/releases/tag";

/// The repository a crates.io entry must name to be this framework, lowercase.
const REPOSITORY: &str = "jalapenolabs/anubis";

/// The URL of `version`'s release notes.
///
/// A release is a tag, and `.github/workflows/release.yml` cuts a GitHub
/// Release from it, so the notes for every published version are one URL away.
///
/// # Examples
/// ```
/// let notes = anubis::upgrade::release_notes(&"0.3.0".parse().unwrap());
/// assert!(notes.ends_with("/releases/tag/v0.3.0"));
/// ```
#[must_use]
pub fn release_notes(version: &Version) -> String {
    format!("{RELEASES}/v{version}")
}

/// What an application resolves the framework from.
///
/// Only [`Dependency::Published`] can be upgraded. The other three are real
/// arrangements with no version to bump: the framework tracked from git (every
/// application stamped before the first release), a local path, and this
/// repository's own starter, which resolves the package through yarn's
/// workspace protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// A published release, named by a requirement such as `^0.2.0`.
    Published(Version),
    /// The framework's git repository.
    Git,
    /// A local directory.
    Path,
    /// Yarn's `workspace:` protocol, which is the monorepo arrangement.
    Workspace,
}

/// One framework dependency, as one manifest declares it.
///
/// Built by [`cargo_declaration`] and [`package_declaration`], which is what
/// ties the borrowed manifest to the byte range the requirement occupies in
/// it. [`Declaration::upgraded`] renders that manifest with the requirement
/// moved to another version.
#[derive(Debug, Clone)]
pub struct Declaration<'a> {
    manifest: &'a str,
    /// The requirement as written, e.g. `^0.2.0` or `{ git = "..." }`.
    requirement: &'a str,
    /// The range `requirement` occupies in `manifest`.
    span: Range<usize>,
    /// The range operator the requirement carries, e.g. `^`. Empty otherwise.
    operator: &'a str,
    dependency: Dependency,
}

impl<'a> Declaration<'a> {
    /// What the application resolves the framework from.
    #[must_use]
    pub fn dependency(&self) -> &Dependency {
        &self.dependency
    }

    /// The requirement exactly as the manifest writes it.
    #[must_use]
    pub fn requirement(&self) -> &'a str {
        self.requirement
    }

    /// The range operator the requirement carries, e.g. `^`, or `""`.
    ///
    /// [`Declaration::upgraded`] keeps it, so this is what a caller prints to
    /// say what the requirement becomes.
    #[must_use]
    pub fn operator(&self) -> &'a str {
        self.operator
    }

    /// The version this declaration names, when it names one.
    #[must_use]
    pub fn version(&self) -> Option<&Version> {
        match &self.dependency {
            Dependency::Published(version) => Some(version),
            Dependency::Git | Dependency::Path | Dependency::Workspace => None,
        }
    }

    /// The manifest again, with the requirement moved to `version`.
    ///
    /// The range operator is preserved, so `^0.2.0` becomes `^0.3.0` and a
    /// bare `0.2.0` stays bare. Everything outside the requirement is copied
    /// through untouched.
    ///
    /// # Errors
    /// Returns an [`UpgradeError`] when the declaration names no version, the
    /// three cases a version cannot be written over: git, a path, and yarn's
    /// workspace protocol.
    pub fn upgraded(&self, version: &Version) -> Result<String, UpgradeError> {
        if self.version().is_none() {
            return Err(UpgradeError::new(format!(
                "`{}` does not name a published version, so there is nothing to bump",
                self.requirement,
            )));
        }

        let mut upgraded = String::with_capacity(self.manifest.len() + 8);
        upgraded.push_str(&self.manifest[..self.span.start]);
        upgraded.push_str(self.operator);
        upgraded.push_str(&version.to_string());
        upgraded.push_str(&self.manifest[self.span.end..]);
        Ok(upgraded)
    }

    /// Builds a declaration over the requirement `span` names in `manifest`.
    fn parse(manifest: &'a str, span: Range<usize>) -> Result<Self, UpgradeError> {
        let requirement = &manifest[span.clone()];
        let (dependency, operator) = classify(requirement)?;
        Ok(Self {
            manifest,
            requirement,
            span,
            operator,
            dependency,
        })
    }

    /// Builds a declaration whose kind the manifest's structure already told
    /// us, which is how Cargo's `{ git = ... }` and `{ path = ... }` arrive.
    fn known(manifest: &'a str, span: Range<usize>, dependency: Dependency) -> Self {
        Self {
            manifest,
            requirement: &manifest[span.clone()],
            span,
            operator: "",
            dependency,
        }
    }
}

/// Reads the `anubis` dependency out of an application's `backend/Cargo.toml`.
///
/// Three shapes are recognized under `[dependencies]`: a bare version string,
/// an inline table carrying `version`, and an inline table carrying `git` or
/// `path`. A `[dependencies.anubis]` table and a multi-line inline table are
/// refused by name, because rewriting either would mean re-emitting TOML this
/// command did not write.
///
/// # Errors
/// Returns an [`UpgradeError`] when the manifest declares no `anubis`
/// dependency, or declares one in a shape this command will not rewrite.
pub fn cargo_declaration(manifest: &str) -> Result<Declaration<'_>, UpgradeError> {
    let entry = cargo_entry(manifest)?;
    let value = &manifest[entry.clone()];

    if value.starts_with('"') {
        let quoted = quoted_span(value).ok_or_else(|| {
            UpgradeError::new(format!(
                "backend/Cargo.toml declares `{CRATE} = {value}`, which is not a closed string",
            ))
        })?;
        return Declaration::parse(manifest, shift(quoted, entry.start));
    }

    if !value.starts_with('{') {
        return Err(UpgradeError::new(format!(
            "backend/Cargo.toml declares `{CRATE} = {value}`, which is neither a version string \
             nor an inline table. Write it as `{CRATE} = {{ version = \"0.1.0\" }}` and run this \
             again.",
        )));
    }
    if !value.ends_with('}') {
        return Err(UpgradeError::new(format!(
            "backend/Cargo.toml spreads the `{CRATE}` dependency over more than one line. Put it \
             on one line, or set the version by hand.",
        )));
    }

    if let Some(version) = assigned_string(value, "version") {
        return Declaration::parse(manifest, shift(version, entry.start));
    }
    for (key, dependency) in [("git", Dependency::Git), ("path", Dependency::Path)] {
        if assigned_string(value, key).is_some() {
            return Ok(Declaration::known(manifest, entry, dependency));
        }
    }

    Err(UpgradeError::new(format!(
        "backend/Cargo.toml declares `{CRATE} = {value}`, which names no version, git, or path \
         source.",
    )))
}

/// Reads the `@jalapenolabs/anubis` dependency out of a `package.json`.
///
/// The value is a string, which is every shape npm accepts: a range such as
/// `^0.2.0`, a git URL, and yarn's `workspace:` protocol.
///
/// # Errors
/// Returns an [`UpgradeError`] when the manifest names no such dependency, or
/// gives it something other than a plain string.
pub fn package_declaration(manifest: &str) -> Result<Declaration<'_>, UpgradeError> {
    let key = format!("\"{PACKAGE}\"");
    let at = manifest.find(&key).ok_or_else(|| {
        UpgradeError::new(format!(
            "frontend/package.json declares no `{PACKAGE}` dependency, so this is not an Anubis \
             application's frontend.",
        ))
    })?;

    let rest = manifest[at + key.len()..].trim_start();
    let rest = rest.strip_prefix(':').ok_or_else(|| {
        UpgradeError::new(format!(
            "frontend/package.json names `{PACKAGE}` outside a dependency entry.",
        ))
    })?;
    let value = rest.trim_start();
    let quoted = quoted_span(value).ok_or_else(|| {
        UpgradeError::new(format!(
            "frontend/package.json gives `{PACKAGE}` something other than a version string.",
        ))
    })?;

    Declaration::parse(manifest, shift(quoted, manifest.len() - value.len()))
}

/// The name `[package] name` declares, which `cargo run -p` needs.
///
/// # Errors
/// Returns an [`UpgradeError`] when the manifest declares no package name.
pub fn crate_name(manifest: &str) -> Result<&str, UpgradeError> {
    let mut section = "";
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = header;
            continue;
        }
        if section != "package" {
            continue;
        }
        if let Some((_at, value)) = assignment(line, "name")
            && let Some(quoted) = quoted_span(value)
        {
            return Ok(&value[quoted]);
        }
    }

    Err(UpgradeError::new(
        "backend/Cargo.toml declares no package name under `[package]`".to_owned(),
    ))
}

/// What crates.io has to say about the `anubis` name.
///
/// The third answer is not hypothetical: the name is held on crates.io by an
/// unrelated crate first published in 2019, so a version read from the
/// registry has to be shown to be this framework's before an application is
/// moved to it. Upgrading an application onto a stranger's crate is not a
/// mistake anyone recovers from by reading the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    /// The registry carries this framework, at this version.
    Published(Version),
    /// The registry carries no crate by this name.
    Absent,
    /// The name is held by a crate that is not this framework.
    Foreign {
        /// The repository that crate names, when it names one.
        repository: Option<String>,
    },
}

/// What crates.io answered, from one API response body.
///
/// The stable version is preferred over the newest one, so a prerelease is
/// never what a plain `anubis upgrade` moves an application to. Identity is
/// decided by the repository the crate declares: anything else is
/// [`Latest::Foreign`], because a name match alone proves nothing.
///
/// # Errors
/// Returns an [`UpgradeError`] when the body is not the JSON the registry
/// documents, or names a version that is not semver.
///
/// # Examples
/// ```
/// use anubis::upgrade::{Latest, latest_published};
///
/// let body = r#"{"crate":{
///     "repository":"https://github.com/JalapenoLabs/Anubis",
///     "max_stable_version":"0.3.0","newest_version":"0.4.0-rc.1"}}"#;
/// assert_eq!(
///     latest_published(body).unwrap(),
///     Latest::Published("0.3.0".parse().unwrap()),
/// );
/// ```
pub fn latest_published(body: &str) -> Result<Latest, UpgradeError> {
    let parsed = serde_json::from_str::<serde_json::Value>(body).map_err(|error| {
        UpgradeError::new(format!("crates.io answered with invalid JSON: {error}"))
    })?;

    if parsed.get("errors").is_some() {
        return Ok(Latest::Absent);
    }

    let Some(details) = parsed.get("crate") else {
        return Err(UpgradeError::new(
            "crates.io answered without a `crate` member".to_owned(),
        ));
    };

    let repository = details
        .get("repository")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if !repository
        .as_deref()
        .is_some_and(|declared| declared.to_ascii_lowercase().contains(REPOSITORY))
    {
        return Ok(Latest::Foreign { repository });
    }

    let named = details
        .get("max_stable_version")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            details
                .get("newest_version")
                .and_then(serde_json::Value::as_str)
        })
        .ok_or_else(|| UpgradeError::new("crates.io named no version for anubis".to_owned()))?;

    Version::parse(named)
        .map(Latest::Published)
        .map_err(|error| {
            UpgradeError::new(format!(
                "crates.io named `{named}`, which is not a version: {error}"
            ))
        })
}

/// The kind of dependency a requirement string names, and its range operator.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "a dependency specifier is not a path, and `.git` on one is written lowercase"
)]
fn classify(requirement: &str) -> Result<(Dependency, &str), UpgradeError> {
    if requirement.starts_with("workspace:") {
        return Ok((Dependency::Workspace, ""));
    }
    if requirement.starts_with("file:") || requirement.starts_with("link:") {
        return Ok((Dependency::Path, ""));
    }
    // Every git spelling npm accepts: a URL, its `git+` and `github:`
    // shorthands, and the bare `owner/repo` form.
    if requirement.contains("://")
        || requirement.starts_with("git+")
        || requirement.starts_with("github:")
        || requirement.ends_with(".git")
    {
        return Ok((Dependency::Git, ""));
    }

    let operator =
        &requirement[..requirement.len() - requirement.trim_start_matches(['^', '~', '=']).len()];
    let named = requirement[operator.len()..].trim();
    let version = Version::parse(named).map_err(|_error| {
        UpgradeError::new(format!(
            "`{requirement}` is a range rather than one release, so this command will not guess \
             what to write in its place. Requirements it rewrites look like `0.2.0`, `^0.2.0`, \
             `~0.2.0`, or `=0.2.0`.",
        ))
    })?;
    Ok((Dependency::Published(version), operator))
}

/// The value assigned to `anubis` under `[dependencies]`, as a byte range.
fn cargo_entry(manifest: &str) -> Result<Range<usize>, UpgradeError> {
    let mut section = "";
    let mut offset = 0;

    for line in manifest.split_inclusive('\n') {
        let start = offset;
        offset += line.len();

        let trimmed = line.trim();
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            if header == "dependencies.anubis" {
                return Err(UpgradeError::new(
                    "backend/Cargo.toml declares anubis as a `[dependencies.anubis]` table. Write \
                     it as one `anubis = { version = \"0.1.0\" }` line under `[dependencies]`, or \
                     set the version by hand."
                        .to_owned(),
                ));
            }
            section = header;
            continue;
        }
        if section != "dependencies" || trimmed.starts_with('#') {
            continue;
        }

        if let Some((at, value)) = assignment(line, CRATE) {
            return Ok((start + at)..(start + at + value.len()));
        }
    }

    Err(UpgradeError::new(format!(
        "backend/Cargo.toml declares no `{CRATE}` dependency under `[dependencies]`, so this is \
         not an Anubis application's backend.",
    )))
}

/// The value `key` is assigned on `line`, with its offset inside the line.
///
/// Every step trims only from the start, so the remaining text stays a suffix
/// of the line and the offset is the difference in lengths.
fn assignment<'a>(line: &'a str, key: &str) -> Option<(usize, &'a str)> {
    let rest = line.trim_start().strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?;
    let value = rest.trim_start();
    Some((line.len() - value.len(), value.trim_end()))
}

/// The range of the string `key` is assigned inside `text`, quotes excluded.
fn assigned_string(text: &str, key: &str) -> Option<Range<usize>> {
    let mut from = 0;
    while let Some(found) = text[from..].find(key) {
        let at = from + found;
        from = at + key.len();

        // A key is a whole word: `git` must not match inside `git_hash`.
        if text[..at]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || matches!(character, '_' | '-'))
        {
            continue;
        }
        let Some(rest) = text[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim_start();
        let quoted = quoted_span(value)?;
        return Some(shift(quoted, text.len() - value.len()));
    }
    None
}

/// The range of the double-quoted string `text` starts with, quotes excluded.
fn quoted_span(text: &str) -> Option<Range<usize>> {
    let body = text.strip_prefix('"')?;
    // Manifests write these requirements as plain text; an escape would mean
    // the value is not one, so refusing beats unescaping.
    if body.starts_with('\\') {
        return None;
    }
    let end = body.find('"')?;
    Some(1..(1 + end))
}

/// The same range, moved `by` bytes further into a larger string.
fn shift(range: Range<usize>, by: usize) -> Range<usize> {
    (range.start + by)..(range.end + by)
}

/// An upgrade the command refuses to plan, with the reason.
///
/// Upgrading is a one-shot command, so the caller's only sane response to a
/// manifest it cannot read is to print the reason and stop. The message is
/// written for a terminal and names the fix wherever one exists.
pub struct UpgradeError {
    message: String,
    backtrace: Backtrace,
}

impl UpgradeError {
    /// Builds an error carrying `message`, capturing a backtrace.
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }

    /// The reason the upgrade was refused.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for UpgradeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpgradeError")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl Display for UpgradeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for UpgradeError {}

#[cfg(test)]
mod tests {
    use super::{
        Dependency, Latest, cargo_declaration, crate_name, latest_published, package_declaration,
        release_notes,
    };
    use semver::Version;

    fn version(text: &str) -> Version {
        text.parse().expect("the test names a version")
    }

    /// The three Cargo shapes a version can be written in, all rewritten in
    /// place with everything around them untouched.
    #[test]
    fn a_cargo_version_is_rewritten_in_place() {
        for (manifest, expected) in [
            (
                "[package]\nname = \"acme\"\n\n[dependencies]\n# a comment\nanubis = \"0.2.0\"\naxum = \"0.8\"\n",
                "[package]\nname = \"acme\"\n\n[dependencies]\n# a comment\nanubis = \"0.3.0\"\naxum = \"0.8\"\n",
            ),
            (
                "[dependencies]\nanubis = { version = \"^0.2.0\", features = [\"full\"] }\n",
                "[dependencies]\nanubis = { version = \"^0.3.0\", features = [\"full\"] }\n",
            ),
            (
                "[dependencies]\nanubis = \"=0.2.0\"\n",
                "[dependencies]\nanubis = \"=0.3.0\"\n",
            ),
        ] {
            let declaration = cargo_declaration(manifest).expect("the shape is recognized");
            assert_eq!(declaration.version(), Some(&version("0.2.0")));
            assert_eq!(
                declaration.upgraded(&version("0.3.0")).expect("published"),
                expected,
            );
        }
    }

    /// A dependency of another name, and one in another section, are not the
    /// framework's.
    #[test]
    fn only_the_framework_dependency_is_read() {
        let manifest =
            "[dependencies]\nanubis-extra = \"9.9.9\"\n\n[dev-dependencies]\nanubis = \"0.2.0\"\n";
        cargo_declaration(manifest).expect_err("the dependency is not under [dependencies]");
    }

    /// Shapes the command will not guess at are refused by name.
    #[test]
    fn unrewritable_cargo_shapes_are_refused() {
        for manifest in [
            // No dependency at all.
            "[dependencies]\naxum = \"0.8\"\n",
            // The multi-line table form.
            "[dependencies]\nanubis = { version = \"0.2.0\",\n  features = [] }\n",
            // The section form.
            "[dependencies.anubis]\nversion = \"0.2.0\"\n",
            // A source that is neither a version, a git repository, nor a path.
            "[dependencies]\nanubis = { registry = \"internal\" }\n",
        ] {
            let error = cargo_declaration(manifest).expect_err("the shape is refused");
            assert!(!error.message().is_empty(), "the refusal names the shape");
        }

        // A range that is not one release: refused rather than guessed at.
        let ranged = "[dependencies]\nanubis = \">=0.2, <0.4\"\n";
        let error = cargo_declaration(ranged).expect_err("a range is refused");
        assert!(error.message().contains("range"), "{}", error.message());
    }

    /// Git and path sources are read, reported, and never rewritten.
    #[test]
    fn a_git_or_path_source_names_no_version() {
        for (manifest, expected) in [
            (
                "[dependencies]\nanubis = { git = \"https://github.com/JalapenoLabs/Anubis.git\" }\n",
                Dependency::Git,
            ),
            (
                "[dependencies]\nanubis = { path = \"../anubis\" }\n",
                Dependency::Path,
            ),
        ] {
            let declaration = cargo_declaration(manifest).expect("the source is recognized");
            assert_eq!(declaration.dependency(), &expected);
            assert_eq!(declaration.version(), None);
            declaration
                .upgraded(&version("0.3.0"))
                .expect_err("there is no version to bump");
        }
    }

    /// The npm range keeps its operator, and the rest of the JSON is untouched.
    #[test]
    fn an_npm_range_is_rewritten_in_place() {
        let manifest = "{\n  \"dependencies\": {\n    \"@jalapenolabs/anubis\": \"^0.2.0\",\n    \"react\": \"^18.3.1\"\n  }\n}\n";
        let declaration = package_declaration(manifest).expect("the shape is recognized");
        assert_eq!(declaration.requirement(), "^0.2.0");
        assert_eq!(
            declaration.upgraded(&version("0.3.0")).expect("published"),
            "{\n  \"dependencies\": {\n    \"@jalapenolabs/anubis\": \"^0.3.0\",\n    \"react\": \"^18.3.1\"\n  }\n}\n",
        );
    }

    /// The two arrangements that exist today: a stamped application tracking
    /// git, and this repository's own starter on the workspace protocol.
    #[test]
    fn npm_sources_without_a_version_are_recognized() {
        for (requirement, expected) in [
            (
                "https://github.com/JalapenoLabs/Anubis.git#workspace=@jalapenolabs/anubis",
                Dependency::Git,
            ),
            ("workspace:^", Dependency::Workspace),
            ("file:../anubis/frontend", Dependency::Path),
        ] {
            let manifest = format!("{{ \"@jalapenolabs/anubis\": \"{requirement}\" }}");
            let declaration = package_declaration(&manifest).expect("the source is recognized");
            assert_eq!(declaration.dependency(), &expected);
            declaration
                .upgraded(&version("0.3.0"))
                .expect_err("there is no version to bump");
        }

        package_declaration("{ \"react\": \"^18.3.1\" }")
            .expect_err("a frontend without the framework is refused");
    }

    #[test]
    fn the_backend_package_name_is_read() {
        let manifest = "[package]\nname = \"acme-crm\"\nversion = \"0.1.0\"\n";
        assert_eq!(
            crate_name(manifest).expect("a name is declared"),
            "acme-crm"
        );
        crate_name("[package]\nversion = \"0.1.0\"\n").expect_err("an unnamed package is refused");
    }

    /// The registry's three answers: a published crate, a crate it does not
    /// carry, and a body that is not what it documents.
    #[test]
    fn the_registry_answer_is_read() {
        let published = r#"{"crate":{"id":"anubis",
            "repository":"https://github.com/JalapenoLabs/Anubis",
            "max_stable_version":"0.3.0","newest_version":"0.4.0-rc.1"}}"#;
        assert_eq!(
            latest_published(published).expect("valid"),
            Latest::Published(version("0.3.0")),
        );

        // Before the first stable release there is no stable version, so the
        // newest one is what the registry has to say.
        let prerelease = r#"{"crate":{
            "repository":"https://github.com/JalapenoLabs/Anubis.git",
            "max_stable_version":null,"newest_version":"0.1.0-rc.1"}}"#;
        assert_eq!(
            latest_published(prerelease).expect("valid"),
            Latest::Published(version("0.1.0-rc.1")),
        );

        let missing = r#"{"errors":[{"detail":"Not Found"}]}"#;
        assert_eq!(
            latest_published(missing).expect("valid"),
            Latest::Absent,
            "a crate the registry does not carry is not an error",
        );

        // Today's reality: the name is held by an unrelated crate, and a name
        // match alone must never move an application onto it.
        let foreign = r#"{"crate":{"id":"anubis","repository":"https://github.com/qhua948/anubis",
            "max_stable_version":"0.0.2","newest_version":"0.0.2"}}"#;
        assert_eq!(
            latest_published(foreign).expect("valid"),
            Latest::Foreign {
                repository: Some("https://github.com/qhua948/anubis".to_owned()),
            },
        );
        assert_eq!(
            latest_published(r#"{"crate":{"max_stable_version":"9.9.9"}}"#).expect("valid"),
            Latest::Foreign { repository: None },
            "a crate naming no repository proves nothing either",
        );

        latest_published("<html>").expect_err("a body that is not JSON is an error");
        latest_published("{}").expect_err("a body without a crate is an error");
    }

    #[test]
    fn release_notes_point_at_the_tag_the_workflow_cuts() {
        assert_eq!(
            release_notes(&version("0.3.0")),
            "https://github.com/JalapenoLabs/Anubis/releases/tag/v0.3.0",
        );
    }
}
