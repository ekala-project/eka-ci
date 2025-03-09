
fn github_pull(domain: &str, owner: &str, pull: u32) {
   GET("repos/{}/{}/pulls/{}", &domain, &owner, pull)
}
