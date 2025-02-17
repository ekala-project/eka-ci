use std::path::PathBuf;

// TODO: Make lazy static
// TODO: Handle errors better
// TODO: Try many potential locations
//   - user, root, and /run
pub fn eka_dirs() -> xdg::BaseDirectories {
    xdg::BaseDirectories::with_prefix("ekaci").unwrap()
}

pub fn eka_git_tree_dirs() -> PathBuf {
    let mut git_dir = PathBuf::new();
    git_dir.push(eka_dirs().get_runtime_directory().unwrap());
    if !git_dir.exists() {
        std::fs::create_dir_all(&git_dir).unwrap();
    }
    git_dir
}


