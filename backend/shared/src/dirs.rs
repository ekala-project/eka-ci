
// TODO: Make lazy static
// TODO: Handle errors better
// TODO: Try many potential locations
//   - user, root, and /run
pub fn eka_dirs() -> xdg::BaseDirectories {
    xdg::BaseDirectories::with_prefix("ekaci").unwrap()
}

