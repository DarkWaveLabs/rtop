//! The one key table. The help overlay and `--help` read it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyHelp {
    pub keys: &'static str,
    pub action: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySection {
    pub title: &'static str,
    pub keys: &'static [KeyHelp],
}

const fn key(keys: &'static str, action: &'static str) -> KeyHelp {
    KeyHelp { keys, action }
}

/// The content of the key table in section 8, in that order.
pub const KEYMAP: &[KeySection] = &[
    KeySection {
        title: "General",
        keys: &[
            key("q, Ctrl-C", "quit"),
            key("?, F1", "show this help"),
            key("space", "pause or resume"),
            key("+ = / -", "interval ×2 / ×0.5"),
            key("Esc", "clear filter, message"),
        ],
    },
    KeySection {
        title: "Processes",
        keys: &[
            key("↑ ↓ j k", "move the selection"),
            key("PgUp PgDn", "move by 10 rows"),
            key("Home g / End G", "first / last row"),
            key("s", "next sort column"),
            key("r", "reverse the sort"),
            key("t", "tree view on or off"),
            key("c", "name or command line"),
            key("x, F9", "kill selected process"),
        ],
    },
    KeySection {
        title: "Panels",
        keys: &[
            key("1", "show or hide cpu"),
            key("2", "show or hide history"),
            key("3", "show or hide memory"),
            key("4", "show or hide network"),
            key("5", "show or hide storage"),
            key("6", "show or hide zfs"),
            key("7", "show or hide sensors"),
            key("8", "show or hide processes"),
            key("0", "show all panels"),
            key("n", "network: per interface"),
        ],
    },
    KeySection {
        title: "Filter, help and kill prompt",
        keys: &[
            key("/", "start the filter"),
            key("Backspace", "filter: edit"),
            key("Enter / Esc", "filter: keep / clear"),
            key("Esc ? q F1", "close this help"),
            key("y", "kill: send TERM"),
            key("f", "kill: send KILL"),
            key("n, Esc", "kill: cancel"),
        ],
    },
];

/// Pure. Plain text for `--help`: each section title, then "    {keys:<16}{action}" lines.
pub fn usage_keys() -> String {
    KEYMAP
        .iter()
        .map(|section| {
            let mut text = section.title.to_string();
            for KeyHelp { keys, action } in section.keys {
                text.push_str(&format!("\n    {keys:<16}{action}"));
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single keys in a `keys` text such as "q, Ctrl-C" or "+ = / -".
    /// A lone "/" is the filter key; " / " between keys is a separator.
    fn tokens(keys: &str) -> Vec<&str> {
        if keys == "/" {
            return vec!["/"];
        }
        keys.split([',', ' '])
            .filter(|t| !t.is_empty() && *t != "/")
            .collect()
    }

    #[test]
    fn tokens_split_keys_but_keep_the_filter_key() {
        assert_eq!(tokens("q, Ctrl-C"), ["q", "Ctrl-C"]);
        assert_eq!(tokens("+ = / -"), ["+", "=", "-"]);
        assert_eq!(tokens("/"), ["/"]);
    }

    #[test]
    fn keymap_has_the_four_sections_in_order() {
        let titles: Vec<_> = KEYMAP.iter().map(|s| s.title).collect();
        assert_eq!(
            titles,
            [
                "General",
                "Processes",
                "Panels",
                "Filter, help and kill prompt"
            ]
        );
    }

    #[test]
    fn no_section_is_empty() {
        for section in KEYMAP {
            assert!(!section.keys.is_empty(), "{} is empty", section.title);
            for key in section.keys {
                assert!(!key.keys.is_empty(), "empty keys in {}", section.title);
                assert!(!key.action.is_empty(), "empty action for {}", key.keys);
            }
        }
    }

    #[test]
    fn keymap_lists_the_new_and_main_keys() {
        let all: Vec<&str> = KEYMAP
            .iter()
            .flat_map(|s| s.keys)
            .flat_map(|k| tokens(k.keys))
            .collect();
        for key in [
            "?", "F1", "q", "x", "F9", "t", "r", "c", "n", "s", "/", "1", "6", "8", "0", "y", "f",
        ] {
            assert!(all.contains(&key), "missing key {key:?} in {all:?}");
        }
    }

    #[test]
    fn panel_digits_name_each_panel_in_order() {
        let panels = KEYMAP.iter().find(|s| s.title == "Panels").unwrap();
        let names = [
            "cpu",
            "history",
            "memory",
            "network",
            "storage",
            "zfs",
            "sensors",
            "processes",
        ];
        for (digit, name) in (1..=8).zip(names) {
            let row = panels
                .keys
                .iter()
                .find(|k| k.keys == digit.to_string())
                .unwrap_or_else(|| panic!("no row for {digit}"));
            assert!(row.action.ends_with(name), "{digit}: {:?}", row.action);
        }
    }

    #[test]
    fn usage_keys_has_each_title_and_action() {
        let text = usage_keys();
        for section in KEYMAP {
            assert!(
                text.lines().any(|l| l == section.title),
                "missing title {:?} in:\n{text}",
                section.title
            );
            for key in section.keys {
                assert!(text.contains(key.action), "missing {:?}", key.action);
            }
        }
    }

    #[test]
    fn usage_keys_lines_use_a_16_wide_keys_column() {
        let text = usage_keys();
        assert!(
            text.lines()
                .any(|l| l == "    x, F9           kill selected process"),
            "no kill line in:\n{text}"
        );
        assert!(!text.ends_with('\n'));
    }
}
