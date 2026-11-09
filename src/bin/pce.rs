//! `pce` — Peter Commander's editor on its own, for the shell and as
//! `$EDITOR` / git's `core.editor`. Only a launcher: the editor is the
//! `peter-commander` binary installed next to this one, run with
//! `--edit`, so `cargo install peter-commander` brings both without a
//! second copy of the whole program. (A link named `pce` to
//! `peter-commander` works as well: it notices the name itself.)

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `peter-commander` binary beside the launcher at `launcher`.
fn main_binary(launcher: &Path) -> PathBuf {
    launcher.with_file_name(format!("peter-commander{}", std::env::consts::EXE_SUFFIX))
}

fn main() {
    let launcher = std::env::current_exe().unwrap_or_else(|err| {
        eprintln!("pce: cannot find where pce is: {err}");
        std::process::exit(1);
    });
    // Where the launcher really is: on macOS `current_exe` can be a link to
    // it, put somewhere `peter-commander` isn't.
    let launcher = launcher.canonicalize().unwrap_or(launcher);
    let target = main_binary(&launcher);
    let mut command = Command::new(&target);
    command.arg("--edit").args(std::env::args_os().skip(1));
    // On Unix the editor takes this process's place, so git (or whoever
    // waits for the editor) and the terminal deal with it directly.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = command.exec();
        eprintln!("pce: cannot run {}: {err}", target.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("pce: cannot run {}: {err}", target.display());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_is_the_binary_beside_the_launcher() {
        let bin = std::env::temp_dir().join("cargo").join("bin");
        let launcher = bin.join(format!("pce{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(
            main_binary(&launcher),
            bin.join(format!("peter-commander{}", std::env::consts::EXE_SUFFIX))
        );
    }
}
