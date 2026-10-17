//! Optional config file: a flat TOML subset. Pure parser plus path rules.

use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::panel::{PanelId, PanelSet};

/// Every field is `None` when the file does not set it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub interval_ms: Option<u64>,
    /// One of `SORT_NAMES`.
    pub sort: Option<String>,
    pub reverse: Option<bool>,
    pub tree: Option<bool>,
    /// Show the full command line in the process table.
    pub command: Option<bool>,
    /// Panels shown at start.
    pub panels: Option<PanelSet>,
    /// From `temp_unit = "f"`.
    pub fahrenheit: Option<bool>,
    /// From `network = "interfaces"`.
    pub net_detail: Option<bool>,
    /// "line N: unknown key `x`, ignored", one for each unknown key.
    pub warnings: Vec<String>,
}

pub const SORT_NAMES: [&str; 5] = ["cpu", "mem", "io", "pid", "name"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parses the file text. Never panics: every failure is a `ParseError`.
pub fn parse(text: &str) -> Result<Config, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut config = Config::default();
    let mut seen: Vec<&str> = Vec::new();
    // `lines` also removes the `\r` of a `\r\n` line end.
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let error = |message: String| ParseError { line, message };
        let rest = skip_ws(raw);
        if rest.is_empty() || rest.starts_with('#') {
            continue;
        }
        if rest.starts_with('[') {
            return Err(error(
                "tables are not supported; use `key = value` lines only".into(),
            ));
        }
        let (key, rest) = parse_key(rest).map_err(error)?;
        let Some(rest) = skip_ws(rest).strip_prefix('=') else {
            return Err(error(format!("expected `=` after `{key}`")));
        };
        let (value, rest) = parse_value(skip_ws(rest)).map_err(error)?;
        let rest = skip_ws(rest);
        if !(rest.is_empty() || rest.starts_with('#')) {
            return Err(error("text after value is not a comment".into()));
        }
        if seen.contains(&key) {
            return Err(error(format!("duplicate key `{key}`")));
        }
        seen.push(key);
        apply(&mut config, line, key, value).map_err(error)?;
    }
    Ok(config)
}

enum Value {
    Integer(u64),
    Boolean(bool),
    String(String),
    Array(Vec<String>),
}

impl Value {
    fn kind(&self) -> &'static str {
        match self {
            Value::Integer(_) => "an integer",
            Value::Boolean(_) => "a boolean",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
        }
    }
}

fn is_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(is_ws)
}

fn parse_key(s: &str) -> Result<(&str, &str), String> {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(s.len());
    if end == 0 {
        return Err("expected a key (letters, digits, `_` or `-`)".into());
    }
    // `end` comes from `find` on a char, so it is a char boundary.
    Ok(s.split_at(end))
}

fn parse_value(s: &str) -> Result<(Value, &str), String> {
    if let Some(rest) = s.strip_prefix('"') {
        let (text, rest) = parse_string(rest)?;
        return Ok((Value::String(text), rest));
    }
    if let Some(rest) = s.strip_prefix('[') {
        return parse_array(rest);
    }
    if let Some(rest) = s.strip_prefix("true") {
        return Ok((Value::Boolean(true), rest));
    }
    if let Some(rest) = s.strip_prefix("false") {
        return Ok((Value::Boolean(false), rest));
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (digits, rest) = s.split_at(end);
        let number = digits
            .parse()
            .map_err(|_| format!("integer `{digits}` is too large"))?;
        return Ok((Value::Integer(number), rest));
    }
    if s.starts_with(['-', '+']) {
        return Err("integer must have no sign".into());
    }
    if s.is_empty() || s.starts_with('#') {
        return Err("missing value".into());
    }
    Err("expected an integer, `true`, `false`, a quoted string or an array".into())
}

/// `s` starts after the opening quote. Returns the text and what follows the closing quote.
fn parse_string(s: &str) -> Result<(String, &str), String> {
    let mut text = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok((text, chars.as_str())),
            '\\' => match chars.next() {
                Some('"') => text.push('"'),
                Some('\\') => text.push('\\'),
                Some(other) => {
                    return Err(format!(
                        "unsupported escape `\\{other}`; only `\\\"` and `\\\\` are allowed"
                    ));
                }
                None => break,
            },
            c if c.is_control() && c != '\t' => {
                return Err("control character in string".into());
            }
            c => text.push(c),
        }
    }
    Err("unterminated string".into())
}

/// `s` starts after the `[`. One line only; items are strings; a trailing comma is allowed.
fn parse_array(s: &str) -> Result<(Value, &str), String> {
    let mut items = Vec::new();
    let mut rest = skip_ws(s);
    loop {
        if let Some(after) = rest.strip_prefix(']') {
            return Ok((Value::Array(items), after));
        }
        if rest.is_empty() || rest.starts_with('#') {
            return Err("unterminated array".into());
        }
        let Some(after) = rest.strip_prefix('"') else {
            return Err("array items must be quoted strings".into());
        };
        let (item, after) = parse_string(after)?;
        items.push(item);
        rest = skip_ws(after);
        if let Some(after) = rest.strip_prefix(',') {
            rest = skip_ws(after);
        } else if rest.is_empty() || rest.starts_with('#') {
            return Err("unterminated array".into());
        } else if !rest.starts_with(']') {
            return Err("expected `,` or `]` in array".into());
        }
    }
}

fn apply(config: &mut Config, line: usize, key: &str, value: Value) -> Result<(), String> {
    match key {
        "interval_ms" => config.interval_ms = Some(expect_integer(key, value)?),
        "sort" => {
            let name = expect_one_of(key, value, &SORT_NAMES)?;
            config.sort = Some(name);
        }
        "reverse" => config.reverse = Some(expect_boolean(key, value)?),
        "tree" => config.tree = Some(expect_boolean(key, value)?),
        "command" => config.command = Some(expect_boolean(key, value)?),
        "panels" => {
            let Value::Array(names) = value else {
                return Err(wrong_type(key, "an array of strings", &value));
            };
            let mut panels = PanelSet::NONE;
            for name in names {
                let Some(id) = PanelId::from_name(&name) else {
                    let known: Vec<&str> = PanelId::ALL.iter().map(|p| p.name()).collect();
                    return Err(format!(
                        "unknown panel `{name}`; expected one of: {}",
                        known.join(", ")
                    ));
                };
                panels.insert(id);
            }
            config.panels = Some(panels);
        }
        "temp_unit" => {
            let unit = expect_one_of(key, value, &["c", "f"])?;
            config.fahrenheit = Some(unit == "f");
        }
        "network" => {
            let view = expect_one_of(key, value, &["total", "interfaces"])?;
            config.net_detail = Some(view == "interfaces");
        }
        _ => config
            .warnings
            .push(format!("line {line}: unknown key `{key}`, ignored")),
    }
    Ok(())
}

fn wrong_type(key: &str, expected: &str, value: &Value) -> String {
    format!("`{key}` must be {expected}, not {}", value.kind())
}

fn expect_integer(key: &str, value: Value) -> Result<u64, String> {
    match value {
        Value::Integer(n) => Ok(n),
        other => Err(wrong_type(key, "an integer", &other)),
    }
}

fn expect_boolean(key: &str, value: Value) -> Result<bool, String> {
    match value {
        Value::Boolean(b) => Ok(b),
        other => Err(wrong_type(key, "a boolean (`true` or `false`)", &other)),
    }
}

fn expect_one_of(key: &str, value: Value, allowed: &[&str]) -> Result<String, String> {
    let Value::String(text) = value else {
        return Err(wrong_type(key, "a string", &value));
    };
    if allowed.contains(&text.as_str()) {
        Ok(text)
    } else {
        Err(format!(
            "unknown value `{text}` for `{key}`; expected one of: {}",
            allowed.join(", ")
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    /// The default path. A missing file is not an error.
    Default,
    /// From `--config`. A missing file is an error.
    Path(PathBuf),
    /// From `--no-config`.
    Disabled,
}

/// Pure. `get` reads an environment variable.
/// Windows: %APPDATA%\rtop\config.toml.
/// Other: $XDG_CONFIG_HOME/rtop/config.toml if it is set and absolute, else $HOME/.config/rtop/config.toml.
pub fn default_path(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let non_empty = |key: &str| get(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    // `cfg!` (not `#[cfg]`) keeps both branches compiled, so neither is dead code on any target.
    let base = if cfg!(windows) {
        non_empty("APPDATA")?
    } else if let Some(xdg) = non_empty("XDG_CONFIG_HOME").filter(|p| p.is_absolute()) {
        xdg
    } else {
        non_empty("HOME")?.join(".config")
    };
    Some(base.join("rtop").join("config.toml"))
}

/// Reads and parses. Returns the config and the path it read (None if no file was read).
/// Error text names the path, and the line for a parse error.
/// `get` reads an environment variable; `ConfigSource::Default` passes it to `default_path`.
/// `main` passes `|k| std::env::var_os(k)`. Tests pass a closure, so they never read the real
/// home directory and never call `set_var` (which is `unsafe` in edition 2024).
pub fn load(
    source: &ConfigSource,
    get: impl Fn(&str) -> Option<OsString>,
) -> anyhow::Result<(Config, Option<PathBuf>)> {
    let path = match source {
        ConfigSource::Disabled => return Ok((Config::default(), None)),
        ConfigSource::Path(path) => path.clone(),
        ConfigSource::Default => match default_path(get) {
            Some(path) => path,
            None => return Ok((Config::default(), None)),
        },
    };
    let text = match read_limited(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound && *source == ConfigSource::Default => {
            return Ok((Config::default(), None));
        }
        Err(e) => return Err(e).with_context(|| format!("read config {}", path.display())),
    };
    let config = parse(&text).with_context(|| format!("config {}", path.display()))?;
    Ok((config, Some(path)))
}

/// The largest config file that `read_limited` accepts.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Reads at most 64 KiB. A larger file is an error.
fn read_limited(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    // Read one byte past the limit, so a larger file shows without reading all of it.
    File::open(path)?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "file is larger than 64 KiB",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "file is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const EXAMPLE: &str = r#"# ~/.config/rtop/config.toml
interval_ms = 500
sort = "mem"
reverse = false
tree = true
command = false
panels = ["cpu", "memory", "network", "zfs", "processes"]
temp_unit = "c"
network = "interfaces"
"#;

    const COMMENTS_ONLY: &str = "# rtop config\n\n   # indented comment\n\t\n";

    const UNKNOWN_KEY: &str = "tree = true\n# colours\ncolour = \"red\"\n";

    /// Each input must fail on the given line.
    const ERROR_CASES: &[(&str, usize)] = &[
        ("tree = true\n[ui]\n", 2),
        ("tree = true\nsort = \"cpu\"\ntree = false\n", 3),
        ("tree = \"yes\"\n", 1),
        ("# comment\nsort = \"fast\"\n", 2),
        ("panels = [\"cpu\", \"gpu\"]\n", 1),
        ("reverse = true\ntemp_unit = \"k\"\n", 2),
        ("name = \"unterminated\n", 1),
        ("\n\npanels = [\"cpu\"\n", 3),
        ("interval_ms = -5\n", 1),
        ("interval_ms = 5 7\n", 1),
        ("sort = \"mem#1\"\n", 1),
        ("sort = \"\\\"cpu\\\"\"\n", 1),
        ("network = \"all\"\n", 1),
        ("interval_ms = 99999999999999999999999\n", 1),
        ("tree = \n", 1),
        ("= true\n", 1),
        ("tree true\n", 1),
        ("panels = [\"cpu\" \"memory\"]\n", 1),
        ("panels = [cpu]\n", 1),
        ("sort = \"c\\pu\"\n", 1),
        ("interval_ms = \"500\"\n", 1),
    ];

    /// Inputs of the other tests, for the one-byte-removed test.
    const OTHER_INPUTS: &[&str] = &[
        "tree = true # yes\n",
        "\u{feff}tree = true\n",
        "panels = []\n",
        "panels = [ \"cpu\" , \"memory\" , ]\n",
        "my-key_2 = [\"a\", \"b\"]\n",
        "a = \"\\\\ and \\\" kept\"\n",
    ];

    fn expected_example() -> Config {
        Config {
            interval_ms: Some(500),
            sort: Some("mem".into()),
            reverse: Some(false),
            tree: Some(true),
            command: Some(false),
            panels: Some(
                [
                    PanelId::Cpu,
                    PanelId::Memory,
                    PanelId::Network,
                    PanelId::Zfs,
                    PanelId::Processes,
                ]
                .into_iter()
                .collect(),
            ),
            fahrenheit: Some(false),
            net_detail: Some(true),
            warnings: Vec::new(),
        }
    }

    /// An environment getter over fixed pairs. Never reads the real environment.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// A unique, empty directory under the system temp dir. Removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "rtop-config-test-{}-{name}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn example_file_parses_to_the_expected_config() {
        let config = parse(EXAMPLE).unwrap();
        assert_eq!(config, expected_example());
        let panels: Vec<PanelId> = config.panels.unwrap().iter().collect();
        assert_eq!(
            panels,
            [
                PanelId::Cpu,
                PanelId::Memory,
                PanelId::Network,
                PanelId::Zfs,
                PanelId::Processes
            ]
        );
    }

    #[test]
    fn empty_and_comment_only_files_give_the_default() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse(COMMENTS_ONLY).unwrap(), Config::default());
    }

    #[test]
    fn leading_bom_is_accepted() {
        let config = parse("\u{feff}tree = true\n").unwrap();
        assert_eq!(config.tree, Some(true));
    }

    #[test]
    fn crlf_line_ends_parse_like_lf() {
        let crlf = EXAMPLE.replace('\n', "\r\n");
        assert_eq!(parse(&crlf), Ok(expected_example()));
    }

    #[test]
    fn inline_comment_after_value_is_ignored() {
        let config = parse("tree = true # yes\n").unwrap();
        assert_eq!(config.tree, Some(true));
    }

    #[test]
    fn hash_inside_string_is_kept() {
        let err = parse("sort = \"mem#1\"\n").unwrap_err();
        assert_eq!(err.line, 1);
        assert!(err.message.contains("mem#1"), "{}", err.message);
    }

    #[test]
    fn errors_report_the_right_line() {
        for &(input, line) in ERROR_CASES {
            match parse(input) {
                Ok(config) => panic!("{input:?} parsed to {config:?}"),
                Err(err) => assert_eq!(err.line, line, "{input:?}: {err}"),
            }
        }
    }

    #[test]
    fn error_messages_name_the_problem() {
        let message = |input: &str| parse(input).unwrap_err().message;
        assert!(message("[ui]\n").contains("table"));
        assert!(message("tree = true\ntree = true\n").contains("duplicate key `tree`"));
        assert!(message("tree = \"yes\"\n").contains("boolean"));
        assert!(message("sort = \"fast\"\n").contains("unknown value `fast`"));
        assert!(message("panels = [\"cpu\", \"gpu\"]\n").contains("unknown panel `gpu`"));
        assert!(message("temp_unit = \"k\"\n").contains("unknown value `k`"));
        assert!(message("name = \"unterminated\n").contains("unterminated string"));
        assert!(message("panels = [\"cpu\"\n").contains("unterminated array"));
        assert!(message("interval_ms = -5\n").contains("sign"));
        assert!(message("interval_ms = 5 7\n").contains("text after value"));
    }

    #[test]
    fn display_shows_the_line() {
        let err = ParseError {
            line: 4,
            message: "duplicate key `tree`".into(),
        };
        assert_eq!(err.to_string(), "line 4: duplicate key `tree`");
    }

    #[test]
    fn unknown_key_is_a_warning() {
        let config = parse(UNKNOWN_KEY).unwrap();
        assert_eq!(config.warnings, ["line 3: unknown key `colour`, ignored"]);
        assert_eq!(config.tree, Some(true));
    }

    #[test]
    fn escaped_quotes_stay_in_the_value() {
        let err = parse("sort = \"\\\"cpu\\\"\"\n").unwrap_err();
        assert!(
            err.message.contains("unknown value `\"cpu\"`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn escapes_and_odd_spacing_parse() {
        let config = parse("a = \"\\\\ and \\\" kept\"\n").unwrap();
        assert_eq!(config.warnings, ["line 1: unknown key `a`, ignored"]);
        let config = parse("panels = [ \"cpu\" , \"memory\" , ]\n").unwrap();
        let both: PanelSet = [PanelId::Cpu, PanelId::Memory].into_iter().collect();
        assert_eq!(config.panels, Some(both));
        assert_eq!(parse("panels = []\n").unwrap().panels, Some(PanelSet::NONE));
        assert_eq!(
            parse("temp_unit = \"f\"\nnetwork = \"total\"\n").unwrap(),
            Config {
                fahrenheit: Some(true),
                net_detail: Some(false),
                ..Config::default()
            }
        );
    }

    #[test]
    fn every_sort_name_is_accepted() {
        for name in SORT_NAMES {
            let config = parse(&format!("sort = \"{name}\"\n")).unwrap();
            assert_eq!(config.sort.as_deref(), Some(name));
        }
    }

    #[test]
    fn sort_names_has_five_unique_entries() {
        let mut names = SORT_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 5);
    }

    #[test]
    fn removing_any_byte_never_panics() {
        let mut inputs: Vec<&str> = vec![EXAMPLE, COMMENTS_ONLY, UNKNOWN_KEY];
        inputs.extend(ERROR_CASES.iter().map(|&(input, _)| input));
        inputs.extend(OTHER_INPUTS);
        for input in inputs {
            let mut cases: Vec<&str> = input.lines().collect();
            cases.push(input);
            for case in cases {
                let bytes = case.as_bytes();
                for i in 0..bytes.len() {
                    let mut cut = bytes.to_vec();
                    cut.remove(i);
                    let _ = parse(&String::from_utf8_lossy(&cut));
                }
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn default_path_follows_xdg_then_home() {
        let path = |pairs: &[(&str, &str)]| default_path(env(pairs));
        assert_eq!(
            path(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]),
            Some(PathBuf::from("/x/rtop/config.toml"))
        );
        assert_eq!(
            path(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")]),
            Some(PathBuf::from("/h/.config/rtop/config.toml"))
        );
        assert_eq!(
            path(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")]),
            Some(PathBuf::from("/h/.config/rtop/config.toml"))
        );
        assert_eq!(
            path(&[("HOME", "/h")]),
            Some(PathBuf::from("/h/.config/rtop/config.toml"))
        );
        assert_eq!(path(&[("XDG_CONFIG_HOME", "rel")]), None);
        assert_eq!(path(&[("HOME", "")]), None);
        assert_eq!(path(&[]), None);
    }

    #[cfg(windows)]
    #[test]
    fn default_path_uses_appdata() {
        let path = |pairs: &[(&str, &str)]| default_path(env(pairs));
        assert_eq!(
            path(&[("APPDATA", r"C:\Users\u\AppData\Roaming"), ("HOME", "/h")]),
            Some(PathBuf::from(
                r"C:\Users\u\AppData\Roaming\rtop\config.toml"
            ))
        );
        assert_eq!(path(&[("APPDATA", "")]), None);
        assert_eq!(path(&[("HOME", "/h")]), None);
    }

    #[test]
    fn disabled_source_reads_nothing() {
        let (config, path) = load(&ConfigSource::Disabled, |_| unreachable!()).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(path, None);
    }

    #[test]
    fn missing_explicit_path_is_an_error_that_names_it() {
        let dir = TempDir::new("missing");
        let missing = dir.0.join("nope.toml");
        let err = load(&ConfigSource::Path(missing.clone()), env(&[])).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains(&missing.display().to_string()), "{text}");
    }

    #[test]
    fn explicit_path_parses_and_parse_errors_name_path_and_line() {
        let dir = TempDir::new("explicit");
        let good = dir.0.join("good.toml");
        fs::write(&good, EXAMPLE).unwrap();
        let (config, path) = load(&ConfigSource::Path(good.clone()), env(&[])).unwrap();
        assert_eq!(config, expected_example());
        assert_eq!(path, Some(good));

        let bad = dir.0.join("bad.toml");
        fs::write(&bad, "tree = true\ntree = false\n").unwrap();
        let err = load(&ConfigSource::Path(bad.clone()), env(&[])).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains(&bad.display().to_string()), "{text}");
        assert!(text.contains("line 2"), "{text}");
    }

    #[test]
    fn default_source_with_no_file_gives_the_default() {
        let dir = TempDir::new("default-empty");
        let home = dir.0.to_str().unwrap();
        // Windows reads %APPDATA%, other targets read $HOME.
        let var = if cfg!(windows) { "APPDATA" } else { "HOME" };
        let (config, path) = load(&ConfigSource::Default, env(&[(var, home)])).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(path, None);
    }

    #[test]
    fn default_source_reads_the_file_under_home() {
        let dir = TempDir::new("default-file");
        let home = dir.0.to_str().unwrap();
        let (var, file) = if cfg!(windows) {
            ("APPDATA", dir.0.join("rtop").join("config.toml"))
        } else {
            (
                "HOME",
                dir.0.join(".config").join("rtop").join("config.toml"),
            )
        };
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, EXAMPLE).unwrap();
        let (config, path) = load(&ConfigSource::Default, env(&[(var, home)])).unwrap();
        assert_eq!(config, expected_example());
        assert_eq!(path, Some(file));
    }

    #[test]
    fn read_limited_refuses_more_than_64_kib() {
        let dir = TempDir::new("limit");
        let at_limit = dir.0.join("at.toml");
        fs::write(&at_limit, "#".repeat(64 * 1024)).unwrap();
        assert_eq!(read_limited(&at_limit).unwrap().len(), 64 * 1024);

        let over = dir.0.join("over.toml");
        fs::write(&over, "#".repeat(64 * 1024 + 1)).unwrap();
        assert!(read_limited(&over).is_err());
    }
}
