//! Model-name inflection: one input name, every casing and plural variant.
//!
//! Scaffolding rewrites template identifiers (`TangibleThing`,
//! `tangible_things`, `tangible-thing`, ...) into the target model's names, so
//! every casing and plural form of a name must be derivable from one input.
//! [`Names::parse`] accepts a model name in `PascalCase`, `camelCase`,
//! `snake_case`, or `kebab-case` and derives all of them.
//!
//! Pluralization covers standard English rules plus a table of the irregular
//! and uncountable words that show up in real domain models. It intentionally
//! stays smaller than a full linguistic engine: an unknown irregular word
//! pluralizes with the default rules, and the generated code is plain source
//! the developer can rename afterward.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

/// Every casing and plural variant of one model name.
///
/// # Examples
/// ```
/// let names = anubis::scaffold::Names::parse("TangibleThing").unwrap();
/// assert_eq!(names.pascal(), "TangibleThing");
/// assert_eq!(names.snake_plural(), "tangible_things");
/// assert_eq!(names.kebab(), "tangible-thing");
/// assert_eq!(names.title(), "Tangible Thing");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// Lowercase words of the singular name, e.g. `["tangible", "thing"]`.
    words: Vec<String>,
    /// The same words with the last one pluralized.
    plural_words: Vec<String>,
}

impl Names {
    /// Parses a model name given in any supported casing.
    ///
    /// # Errors
    /// Returns an error when the name is empty, does not start with an ASCII
    /// letter, or contains characters other than ASCII letters, digits,
    /// underscores, and hyphens.
    pub fn parse(input: &str) -> Result<Self, NameError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(NameError::new("model name is empty".to_owned()));
        }
        if !trimmed.starts_with(|character: char| character.is_ascii_alphabetic()) {
            return Err(NameError::new(format!(
                "model name `{trimmed}` must start with an ASCII letter"
            )));
        }
        if let Some(bad) = trimmed.chars().find(|character| {
            !character.is_ascii_alphanumeric() && *character != '_' && *character != '-'
        }) {
            return Err(NameError::new(format!(
                "model name `{trimmed}` contains unsupported character `{bad}`"
            )));
        }

        let mut words = Vec::new();
        for chunk in trimmed.split(['_', '-']) {
            if chunk.is_empty() {
                return Err(NameError::new(format!(
                    "model name `{trimmed}` has an empty word segment"
                )));
            }
            split_case_boundaries(chunk, &mut words);
        }

        let mut plural_words = words.clone();
        if let Some(last) = plural_words.last_mut() {
            *last = pluralize(last);
        }

        Ok(Self {
            words,
            plural_words,
        })
    }

    /// The singular `PascalCase` form, e.g. `TangibleThing`.
    #[must_use]
    pub fn pascal(&self) -> String {
        self.words.iter().map(|word| capitalize(word)).collect()
    }

    /// The plural `PascalCase` form, e.g. `TangibleThings`.
    #[must_use]
    pub fn pascal_plural(&self) -> String {
        self.plural_words
            .iter()
            .map(|word| capitalize(word))
            .collect()
    }

    /// The singular `camelCase` form, e.g. `tangibleThing`.
    #[must_use]
    pub fn camel(&self) -> String {
        camel_join(&self.words)
    }

    /// The plural `camelCase` form, e.g. `tangibleThings`.
    #[must_use]
    pub fn camel_plural(&self) -> String {
        camel_join(&self.plural_words)
    }

    /// The singular `snake_case` form, e.g. `tangible_thing`.
    #[must_use]
    pub fn snake(&self) -> String {
        self.words.join("_")
    }

    /// The plural `snake_case` form, e.g. `tangible_things`.
    #[must_use]
    pub fn snake_plural(&self) -> String {
        self.plural_words.join("_")
    }

    /// The singular `SCREAMING_SNAKE_CASE` form, e.g. `TANGIBLE_THING`.
    ///
    /// This is the form module-level constants take in both Rust and
    /// TypeScript, as in `TANGIBLE_THING_MODEL`.
    #[must_use]
    pub fn screaming(&self) -> String {
        self.snake().to_ascii_uppercase()
    }

    /// The plural `SCREAMING_SNAKE_CASE` form, e.g. `TANGIBLE_THINGS`.
    #[must_use]
    pub fn screaming_plural(&self) -> String {
        self.snake_plural().to_ascii_uppercase()
    }

    /// The singular `kebab-case` form, e.g. `tangible-thing`.
    #[must_use]
    pub fn kebab(&self) -> String {
        self.words.join("-")
    }

    /// The plural `kebab-case` form, e.g. `tangible-things`.
    #[must_use]
    pub fn kebab_plural(&self) -> String {
        self.plural_words.join("-")
    }

    /// The singular `Title Case` form, e.g. `Tangible Thing`.
    #[must_use]
    pub fn title(&self) -> String {
        self.words
            .iter()
            .map(|word| capitalize(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The plural `Title Case` form, e.g. `Tangible Things`.
    #[must_use]
    pub fn title_plural(&self) -> String {
        self.plural_words
            .iter()
            .map(|word| capitalize(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The singular sentence form, e.g. `Tangible thing`.
    #[must_use]
    pub fn human(&self) -> String {
        let mut sentence = self.words.join(" ");
        if let Some(first) = sentence.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        sentence
    }

    /// The singular spaced lowercase form, e.g. `tangible thing`.
    ///
    /// This is the form prose uses mid-sentence, as in `Name the tangible
    /// thing.`, so templates can write natural messages that still transform.
    #[must_use]
    pub fn lower(&self) -> String {
        self.words.join(" ")
    }

    /// The plural spaced lowercase form, e.g. `tangible things`.
    #[must_use]
    pub fn lower_plural(&self) -> String {
        self.plural_words.join(" ")
    }
}

/// Splits `chunk` on lowercase-to-uppercase boundaries into lowercase words.
///
/// Consecutive capitals stay one word (`HTTPServer` becomes `http` +
/// `server`), matching how identifiers read in practice.
fn split_case_boundaries(chunk: &str, words: &mut Vec<String>) {
    let characters = chunk.chars().collect::<Vec<_>>();
    let mut current = String::new();
    for (index, character) in characters.iter().enumerate() {
        if character.is_ascii_uppercase() && !current.is_empty() {
            let previous_is_lower = characters[index - 1].is_ascii_lowercase()
                || characters[index - 1].is_ascii_digit();
            let next_is_lower = characters
                .get(index + 1)
                .is_some_and(char::is_ascii_lowercase);
            if previous_is_lower || next_is_lower {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
}

fn capitalize(word: &str) -> String {
    let mut capitalized = word.to_owned();
    if let Some(first) = capitalized.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    capitalized
}

fn camel_join(words: &[String]) -> String {
    let mut joined = String::new();
    for (index, word) in words.iter().enumerate() {
        if index == 0 {
            joined.push_str(word);
        } else {
            joined.push_str(&capitalize(word));
        }
    }
    joined
}

/// Words that read the same in singular and plural.
const UNCOUNTABLE: [&str; 8] = [
    "data",
    "deer",
    "equipment",
    "fish",
    "information",
    "series",
    "sheep",
    "species",
];

/// Singular-to-plural pairs the suffix rules cannot derive.
const IRREGULAR: [(&str, &str); 18] = [
    ("child", "children"),
    ("criterion", "criteria"),
    ("datum", "data"),
    ("echo", "echoes"),
    ("foot", "feet"),
    ("goose", "geese"),
    ("half", "halves"),
    ("hero", "heroes"),
    ("knife", "knives"),
    ("leaf", "leaves"),
    ("life", "lives"),
    ("man", "men"),
    ("mouse", "mice"),
    ("person", "people"),
    ("potato", "potatoes"),
    ("shelf", "shelves"),
    ("tomato", "tomatoes"),
    ("tooth", "teeth"),
];

/// Pluralizes one lowercase English word.
///
/// # Examples
/// ```
/// assert_eq!(anubis::scaffold::pluralize("project"), "projects");
/// assert_eq!(anubis::scaffold::pluralize("category"), "categories");
/// assert_eq!(anubis::scaffold::pluralize("status"), "statuses");
/// assert_eq!(anubis::scaffold::pluralize("person"), "people");
/// ```
#[must_use]
pub fn pluralize(word: &str) -> String {
    if UNCOUNTABLE.contains(&word) {
        return word.to_owned();
    }
    if let Some((_, plural)) = IRREGULAR.iter().find(|(singular, _)| *singular == word) {
        return (*plural).to_owned();
    }
    if word.ends_with("quiz") {
        return format!("{word}zes");
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|suffix| word.ends_with(suffix))
    {
        return format!("{word}es");
    }
    if let Some(stem) = word.strip_suffix('y') {
        let vowel_before = stem.ends_with(['a', 'e', 'i', 'o', 'u']);
        if !vowel_before && !stem.is_empty() {
            return format!("{stem}ies");
        }
    }
    format!("{word}s")
}

/// A rejected model name, with the reason.
pub struct NameError {
    message: String,
    backtrace: Backtrace,
}

impl NameError {
    fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }

    /// The reason the name was rejected.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for NameError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("NameError")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl Display for NameError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for NameError {}

#[cfg(test)]
mod tests {
    use super::{Names, pluralize};

    #[test]
    fn parses_every_supported_casing_to_the_same_names() {
        for input in [
            "TangibleThing",
            "tangibleThing",
            "tangible_thing",
            "tangible-thing",
        ] {
            let names = Names::parse(input).unwrap();
            assert_eq!(names.pascal(), "TangibleThing", "input {input}");
            assert_eq!(names.snake(), "tangible_thing", "input {input}");
        }
    }

    #[test]
    fn derives_all_variants() {
        let names = Names::parse("CreativeConcept").unwrap();
        assert_eq!(names.pascal(), "CreativeConcept");
        assert_eq!(names.pascal_plural(), "CreativeConcepts");
        assert_eq!(names.camel(), "creativeConcept");
        assert_eq!(names.camel_plural(), "creativeConcepts");
        assert_eq!(names.snake(), "creative_concept");
        assert_eq!(names.snake_plural(), "creative_concepts");
        assert_eq!(names.screaming(), "CREATIVE_CONCEPT");
        assert_eq!(names.screaming_plural(), "CREATIVE_CONCEPTS");
        assert_eq!(names.kebab(), "creative-concept");
        assert_eq!(names.kebab_plural(), "creative-concepts");
        assert_eq!(names.title(), "Creative Concept");
        assert_eq!(names.title_plural(), "Creative Concepts");
        assert_eq!(names.human(), "Creative concept");
        assert_eq!(names.lower(), "creative concept");
        assert_eq!(names.lower_plural(), "creative concepts");
    }

    #[test]
    fn single_word_names_work() {
        let names = Names::parse("Project").unwrap();
        assert_eq!(names.pascal(), "Project");
        assert_eq!(names.snake_plural(), "projects");
        assert_eq!(names.camel(), "project");
    }

    #[test]
    fn acronym_runs_stay_one_word() {
        let names = Names::parse("HTTPServer").unwrap();
        assert_eq!(names.snake(), "http_server");
        assert_eq!(names.pascal(), "HttpServer");
    }

    #[test]
    fn rejects_bad_names() {
        Names::parse("").unwrap_err();
        Names::parse("9lives").unwrap_err();
        Names::parse("bad name").unwrap_err();
        Names::parse("semi;colon").unwrap_err();
        Names::parse("double__underscore").unwrap_err();
    }

    #[test]
    fn pluralization_rules() {
        let cases = [
            ("project", "projects"),
            ("category", "categories"),
            ("company", "companies"),
            ("day", "days"),
            ("status", "statuses"),
            ("box", "boxes"),
            ("batch", "batches"),
            ("dish", "dishes"),
            ("quiz", "quizzes"),
            ("person", "people"),
            ("child", "children"),
            ("leaf", "leaves"),
            ("roof", "roofs"),
            ("chef", "chefs"),
            ("hero", "heroes"),
            ("photo", "photos"),
            ("equipment", "equipment"),
            ("series", "series"),
        ];
        for (singular, plural) in cases {
            assert_eq!(pluralize(singular), plural, "pluralize({singular})");
        }
    }
}
