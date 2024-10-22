use crate::error::Result;
use crate::utils;

/// Clone TileDB-Inc/TileDB into `target/repos/tiledb`.
pub fn update() -> Result<()> {
    if utils::git_dir().is_dir() {
        return Ok(());
    }

    let out_dir = utils::out_dir().display().to_string();
    let cmd = [
        "git",
        "-C",
        &out_dir,
        "clone",
        "https://github.com/TileDB-Inc/TileDB",
        "git",
    ]
    .to_vec();

    crate::command::run(&cmd, None)
}
