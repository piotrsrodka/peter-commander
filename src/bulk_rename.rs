//! Bulk Rename: the selected names are written one per line to a temp file,
//! edited in $EDITOR, and the edited lines become the new names. Nothing is
//! renamed until `plan` has validated the whole edit and the user has
//! confirmed it — including any existing files the new names would replace.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::fs_ops;

/// A validated edit: the items whose name actually changed, and which of
/// their new names would replace an existing entry that isn't itself part
/// of the batch.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub renames: Vec<(String, String)>,
    pub overwrites: Vec<String>,
}

/// The text written to the editor file.
pub fn list_text(names: &[String]) -> String {
    let mut text = names.join("\n");
    text.push('\n');
    text
}

/// Matches the edited text up with `old_names`, line by line. Pure, so all
/// the "is this edit acceptable" rules are testable without a filesystem.
pub fn parse_edit(old_names: &[String], edited: &str) -> Result<Vec<(String, String)>, String> {
    let edited = edited.strip_prefix('\u{feff}').unwrap_or(edited);
    let mut lines: Vec<&str> = edited
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // The trailing newline (and any blank lines an editor left at the end)
    // aren't names.
    while lines.last().is_some_and(|line| line.is_empty()) && lines.len() > old_names.len() {
        lines.pop();
    }
    if lines.len() != old_names.len() {
        return Err(format!(
            "Expected {} lines, found {} — lines must not be added or removed",
            old_names.len(),
            lines.len()
        ));
    }

    let case_insensitive = cfg!(any(windows, target_os = "macos"));
    let fold = |name: &str| {
        if case_insensitive {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    };
    let mut seen = HashSet::new();
    for line in &lines {
        validate_name(line)?;
        if !seen.insert(fold(line)) {
            return Err(format!("\"{line}\" appears more than once"));
        }
    }

    Ok(old_names
        .iter()
        .zip(lines)
        .filter(|(old, new)| old.as_str() != *new)
        .map(|(old, new)| (old.clone(), new.to_string()))
        .collect())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("A name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("\"{name}\" is not a valid name"));
    }
    let forbidden: &[char] = if cfg!(windows) {
        &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0']
    } else {
        &['/', '\0']
    };
    if let Some(c) = name.chars().find(|c| forbidden.contains(c)) {
        return Err(format!("\"{name}\" contains the character '{c}'"));
    }
    Ok(())
}

/// `parse_edit` plus a look at `dir`: a new name that's already taken by
/// something *outside* the batch would be overwritten, so it's listed for
/// the confirmation prompt. (A name freed up by another item in the same
/// batch — e.g. a swap — isn't an overwrite.)
pub fn plan(dir: &Path, old_names: &[String], edited: &str) -> Result<Plan, String> {
    let renames = parse_edit(old_names, edited)?;
    let overwrites = renames
        .iter()
        .filter(|(_, new)| {
            let target = dir.join(new);
            renames
                .iter()
                .all(|(old, _)| fs_ops::rename_target_taken(&dir.join(old), &target))
        })
        .map(|(_, new)| new.clone())
        .collect();
    Ok(Plan { renames, overwrites })
}

/// Performs a confirmed plan in two phases — every item to a temporary name
/// first, then to its new name — so swaps and chains (a→b, b→c) work in any
/// order. Only names in `plan.overwrites` (which the user confirmed) may
/// replace an existing entry; anything else that turns up in the way is
/// left alone and reported. Returns how many were renamed and the errors.
pub fn execute(dir: &Path, plan: &Plan) -> (usize, Vec<String>) {
    let mut errors = Vec::new();
    let pid = std::process::id();

    let mut parked = Vec::new();
    for (idx, (old, new)) in plan.renames.iter().enumerate() {
        let Some(temp) = (0..)
            .map(|n| format!(".pc-rename-{pid}-{idx}-{n}"))
            .find(|name| fs::symlink_metadata(dir.join(name)).is_err())
        else {
            continue;
        };
        match fs::rename(dir.join(old), dir.join(&temp)) {
            Ok(()) => parked.push((temp, old, new)),
            Err(err) => errors.push(format!("{old}: {err}")),
        }
    }

    let mut done = 0;
    for (temp, old, new) in parked {
        let target = dir.join(new);
        let allowed = plan.overwrites.contains(new);
        let result = if !allowed && fs::symlink_metadata(&target).is_ok() {
            Err(format!("{old}: \"{new}\" already exists"))
        } else {
            fs::rename(dir.join(&temp), &target).map_err(|err| format!("{old} → {new}: {err}"))
        };
        match result {
            Ok(()) => done += 1,
            Err(message) => errors.push(restore(dir, &temp, old, message)),
        }
    }
    (done, errors)
}

/// Puts a parked item back under its old name — unless something now
/// occupies that name (e.g. another item of a swap already landed there),
/// in which case it stays under its temporary name rather than replacing
/// anything, and the message says where it is.
fn restore(dir: &Path, temp: &str, old: &str, message: String) -> String {
    if fs::symlink_metadata(dir.join(old)).is_err() && fs::rename(dir.join(temp), dir.join(old)).is_ok() {
        message
    } else {
        format!("{message} (left as \"{temp}\")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unchanged_lines_are_skipped_and_crlf_bom_tolerated() {
        let old = names(&["a.txt", "b.txt"]);
        let edited = "\u{feff}a.txt\r\nc.txt\r\n\r\n";
        assert_eq!(
            parse_edit(&old, edited).unwrap(),
            vec![("b.txt".to_string(), "c.txt".to_string())]
        );
    }

    #[test]
    fn rejects_bad_edits() {
        let old = names(&["a", "b"]);
        assert!(parse_edit(&old, "a\n").is_err(), "line removed");
        assert!(parse_edit(&old, "a\nb\nc\n").is_err(), "line added");
        assert!(parse_edit(&old, "a\n\n").is_err(), "empty name");
        assert!(parse_edit(&old, "a\n..\n").is_err(), "dot-dot");
        assert!(parse_edit(&old, "a\nx/y\n").is_err(), "slash");
        assert!(parse_edit(&old, "c\nc\n").is_err(), "duplicate");
        assert!(parse_edit(&old, "b\na\n").is_ok(), "swap");
    }

    fn test_dir(name: &str, files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for file in files {
            fs::write(dir.join(file), file).unwrap();
        }
        dir
    }

    #[test]
    fn swap_is_not_an_overwrite_and_works() {
        let dir = test_dir("pc_test_bulk_swap", &["a", "b"]);
        let plan = plan(&dir, &names(&["a", "b"]), "b\na\n").unwrap();
        assert!(plan.overwrites.is_empty());
        let (done, errors) = execute(&dir, &plan);
        assert_eq!((done, errors.len()), (2, 0));
        assert_eq!(fs::read_to_string(dir.join("a")).unwrap(), "b");
        assert_eq!(fs::read_to_string(dir.join("b")).unwrap(), "a");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_file_outside_the_batch_is_flagged_and_only_replaced_when_planned() {
        let dir = test_dir("pc_test_bulk_overwrite", &["a", "keep"]);
        let old = names(&["a"]);
        let plan_it = plan(&dir, &old, "keep\n").unwrap();
        assert_eq!(plan_it.overwrites, vec!["keep".to_string()]);

        // Not confirmed: the existing file must survive and "a" come back.
        let unconfirmed = Plan {
            renames: plan_it.renames.clone(),
            overwrites: Vec::new(),
        };
        let (done, errors) = execute(&dir, &unconfirmed);
        assert_eq!((done, errors.len()), (0, 1));
        assert_eq!(fs::read_to_string(dir.join("keep")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(dir.join("a")).unwrap(), "a");

        // Confirmed: replaced.
        let (done, errors) = execute(&dir, &plan_it);
        assert_eq!((done, errors.len()), (1, 0));
        assert_eq!(fs::read_to_string(dir.join("keep")).unwrap(), "a");
        assert!(!dir.join("a").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_restore_never_replaces_the_new_occupant_of_the_old_name() {
        // Simulates a swap where the second rename fails after the first
        // already landed on its old name: the parked item must stay parked.
        let dir = test_dir("pc_test_bulk_restore", &["b", ".pc-parked"]);
        let message = restore(&dir, ".pc-parked", "b", "boom".to_string());
        assert!(message.contains("left as"));
        assert_eq!(fs::read_to_string(dir.join("b")).unwrap(), "b");
        assert!(dir.join(".pc-parked").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
