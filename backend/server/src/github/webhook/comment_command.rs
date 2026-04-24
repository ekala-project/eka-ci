//! Parser for `@eka-ci …` PR comment commands.
//!
//! Only `@eka-ci merge [method|cancel]` is recognized. The mention must
//! be the first non-whitespace token of a line so prose mentions don't
//! trigger. Matching is case-insensitive; trailing tokens after the
//! method are ignored (permissive: typos still parse as merge).

/// A parsed comment command. New verbs can be added without breaking
/// callers since the enum is `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommentCommand {
    /// `@eka-ci merge [method]` — queue an SHA-pinned merge.
    Merge { method: Option<MergeMethod> },
    /// `@eka-ci merge cancel` — withdraw a previously-issued merge.
    MergeCancel,
}

/// Merge method argument. Mirrors the three methods GitHub supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// Lowercase form used in DB storage and the GitHub API.
    pub fn as_str(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
}

/// Bot handle (case-insensitive).
const BOT_MENTION: &str = "@eka-ci";

/// Parse a comment body for a supported `@eka-ci` command. Scans each
/// line for a leading mention and returns the first command it finds.
pub fn parse_comment_command(body: &str) -> Option<CommentCommand> {
    for line in body.lines() {
        if let Some(cmd) = parse_line(line) {
            return Some(cmd);
        }
    }
    None
}

fn parse_line(line: &str) -> Option<CommentCommand> {
    let trimmed = line.trim_start();
    let mut tokens = trimmed.split_whitespace();

    // First token: bot mention (case-insensitive).
    let mention = tokens.next()?;
    if !mention.eq_ignore_ascii_case(BOT_MENTION) {
        return None;
    }

    // Second token: command verb. Unknown verbs drop the whole comment.
    let verb = tokens.next()?.to_ascii_lowercase();
    if verb != "merge" {
        return None;
    }

    // Optional third token: method or `cancel`.
    match tokens.next() {
        None => Some(CommentCommand::Merge { method: None }),
        Some(arg) => {
            let arg_lc = arg.to_ascii_lowercase();
            match arg_lc.as_str() {
                "cancel" => Some(CommentCommand::MergeCancel),
                "merge" => Some(CommentCommand::Merge {
                    method: Some(MergeMethod::Merge),
                }),
                "squash" => Some(CommentCommand::Merge {
                    method: Some(MergeMethod::Squash),
                }),
                "rebase" => Some(CommentCommand::Merge {
                    method: Some(MergeMethod::Rebase),
                }),
                // Unknown argument → default merge (permissive).
                _ => Some(CommentCommand::Merge { method: None }),
            }
        },
    }
}

// ---------------------------------------------------------------------
// Per-(user, owner, repo) rate limit. Protects the installation's
// shared 5000 req/hr API budget from comment-driven amplification.
// Rejections are silent — feedback would itself amplify spam.
// State is process-local; restarts clear it.
// ---------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Minimum interval between accepted commands per `(user_id, owner, repo)`.
pub(crate) const COMMAND_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// Map size cap. On overflow, entries older than `2 * COMMAND_MIN_INTERVAL`
/// are pruned opportunistically on insert. 10k entries ≈ <1 MB.
const RATE_LIMIT_MAP_MAX_ENTRIES: usize = 10_000;

type RateLimitKey = (i64, String, String);

static COMMAND_RATE_LIMITS: LazyLock<Mutex<HashMap<RateLimitKey, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Accept-and-record a command from `(user_id, owner, repo)`. Returns
/// `true` to accept, `false` to rate-limit. Fail-open on lock poison.
pub(crate) fn check_and_record_rate_limit(user_id: i64, owner: &str, repo: &str) -> bool {
    check_and_record_rate_limit_inner(
        &COMMAND_RATE_LIMITS,
        user_id,
        owner,
        repo,
        Instant::now(),
        COMMAND_MIN_INTERVAL,
    )
}

/// Pure core; tests pass synthetic map + `Instant` to avoid sleeps.
fn check_and_record_rate_limit_inner(
    map: &Mutex<HashMap<RateLimitKey, Instant>>,
    user_id: i64,
    owner: &str,
    repo: &str,
    now: Instant,
    min_interval: Duration,
) -> bool {
    let mut map = match map.lock() {
        Ok(g) => g,
        Err(_) => return true, // fail-open on poison
    };
    let key = (user_id, owner.to_string(), repo.to_string());

    if let Some(&last) = map.get(&key) {
        if now.saturating_duration_since(last) < min_interval {
            return false;
        }
    }

    map.insert(key, now);

    // Opportunistic prune on insert once the map is over the cap.
    if map.len() > RATE_LIMIT_MAP_MAX_ENTRIES {
        if let Some(cutoff) = now.checked_sub(min_interval * 2) {
            map.retain(|_, ts| *ts > cutoff);
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_merge_no_method() {
        assert_eq!(
            parse_comment_command("@eka-ci merge"),
            Some(CommentCommand::Merge { method: None })
        );
    }

    #[test]
    fn merge_squash() {
        assert_eq!(
            parse_comment_command("@eka-ci merge squash"),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Squash)
            })
        );
    }

    #[test]
    fn merge_rebase() {
        assert_eq!(
            parse_comment_command("@eka-ci merge rebase"),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Rebase)
            })
        );
    }

    #[test]
    fn merge_merge_explicit_method() {
        assert_eq!(
            parse_comment_command("@eka-ci merge merge"),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Merge)
            })
        );
    }

    #[test]
    fn merge_cancel() {
        assert_eq!(
            parse_comment_command("@eka-ci merge cancel"),
            Some(CommentCommand::MergeCancel)
        );
    }

    #[test]
    fn mention_is_case_insensitive() {
        assert_eq!(
            parse_comment_command("@Eka-CI merge"),
            Some(CommentCommand::Merge { method: None })
        );
    }

    #[test]
    fn method_is_case_insensitive() {
        assert_eq!(
            parse_comment_command("@eka-ci MERGE SQUASH"),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Squash)
            })
        );
    }

    #[test]
    fn leading_whitespace_ok() {
        assert_eq!(
            parse_comment_command("   @eka-ci merge"),
            Some(CommentCommand::Merge { method: None })
        );
    }

    #[test]
    fn mention_must_be_line_start() {
        // Mention inside a sentence is ignored; this prevents accidental
        // triggers from comments like "cc @eka-ci please help".
        assert_eq!(parse_comment_command("hey @eka-ci merge"), None);
    }

    #[test]
    fn works_on_multi_line_comment() {
        let body = "LGTM, ship it!\n@eka-ci merge squash\nthanks!";
        assert_eq!(
            parse_comment_command(body),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Squash)
            })
        );
    }

    #[test]
    fn first_matching_line_wins() {
        let body = "@eka-ci merge rebase\n@eka-ci merge cancel";
        assert_eq!(
            parse_comment_command(body),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Rebase)
            })
        );
    }

    #[test]
    fn no_mention_returns_none() {
        assert_eq!(parse_comment_command("merge this pr please"), None);
    }

    #[test]
    fn unknown_verb_returns_none() {
        assert_eq!(parse_comment_command("@eka-ci rebuild"), None);
    }

    #[test]
    fn unknown_method_falls_back_to_default_merge() {
        // We prefer permissive parsing so typos still queue a merge.
        assert_eq!(
            parse_comment_command("@eka-ci merge squashh"),
            Some(CommentCommand::Merge { method: None })
        );
    }

    #[test]
    fn trailing_tokens_after_method_are_ignored() {
        assert_eq!(
            parse_comment_command("@eka-ci merge squash please thanks"),
            Some(CommentCommand::Merge {
                method: Some(MergeMethod::Squash)
            })
        );
    }

    #[test]
    fn empty_comment_returns_none() {
        assert_eq!(parse_comment_command(""), None);
    }

    #[test]
    fn mention_only_returns_none() {
        assert_eq!(parse_comment_command("@eka-ci"), None);
    }

    #[test]
    fn merge_method_as_str_round_trip() {
        assert_eq!(MergeMethod::Merge.as_str(), "merge");
        assert_eq!(MergeMethod::Squash.as_str(), "squash");
        assert_eq!(MergeMethod::Rebase.as_str(), "rebase");
    }

    // -----------------------------------------------------------------
    // Rate limiter tests. These exercise `check_and_record_rate_limit_inner`
    // with a caller-supplied map and synthetic `Instant`s so we can test
    // interval semantics deterministically without sleeps.
    // -----------------------------------------------------------------

    fn fresh_map() -> Mutex<HashMap<RateLimitKey, Instant>> {
        Mutex::new(HashMap::new())
    }

    #[test]
    fn rate_limit_accepts_first_request() {
        let map = fresh_map();
        let t0 = Instant::now();
        let accepted = check_and_record_rate_limit_inner(
            &map,
            42,
            "acme",
            "widgets",
            t0,
            Duration::from_secs(5),
        );
        assert!(accepted, "first request from a new key should be accepted");
        assert_eq!(map.lock().unwrap().len(), 1);
    }

    #[test]
    fn rate_limit_rejects_immediate_retry() {
        let map = fresh_map();
        let t0 = Instant::now();
        let min = Duration::from_secs(5);

        assert!(check_and_record_rate_limit_inner(
            &map, 42, "a", "r", t0, min
        ));
        // Second call 1s later is well inside the 5s window.
        let t1 = t0 + Duration::from_secs(1);
        assert!(
            !check_and_record_rate_limit_inner(&map, 42, "a", "r", t1, min),
            "second request inside the interval should be rejected"
        );
    }

    #[test]
    fn rate_limit_accepts_after_interval_elapses() {
        let map = fresh_map();
        let t0 = Instant::now();
        let min = Duration::from_secs(5);

        assert!(check_and_record_rate_limit_inner(
            &map, 42, "a", "r", t0, min
        ));
        // Just past the interval boundary.
        let t1 = t0 + Duration::from_secs(5) + Duration::from_millis(1);
        assert!(
            check_and_record_rate_limit_inner(&map, 42, "a", "r", t1, min),
            "request after the interval should be accepted"
        );
    }

    #[test]
    fn rate_limit_keys_differ_by_user() {
        let map = fresh_map();
        let t0 = Instant::now();
        let min = Duration::from_secs(5);

        assert!(check_and_record_rate_limit_inner(
            &map, 42, "a", "r", t0, min
        ));
        // Different user at same instant must not inherit user 42's limit.
        assert!(
            check_and_record_rate_limit_inner(&map, 43, "a", "r", t0, min),
            "different user should have an independent bucket"
        );
    }

    #[test]
    fn rate_limit_keys_differ_by_repo() {
        let map = fresh_map();
        let t0 = Instant::now();
        let min = Duration::from_secs(5);

        assert!(check_and_record_rate_limit_inner(
            &map, 42, "a", "r1", t0, min
        ));
        assert!(
            check_and_record_rate_limit_inner(&map, 42, "a", "r2", t0, min),
            "same user on a different repo should have an independent bucket"
        );
        assert!(
            check_and_record_rate_limit_inner(&map, 42, "b", "r1", t0, min),
            "same user on a different owner should have an independent bucket"
        );
    }

    #[test]
    fn rate_limit_prunes_when_oversized() {
        // Verify opportunistic pruning fires on insert once the map
        // exceeds `RATE_LIMIT_MAP_MAX_ENTRIES`. Construct a map just
        // over the threshold with stale timestamps, then trigger a
        // fresh insert with `now` well past `2 * min_interval` so the
        // retain pass drops everything old.
        let map = fresh_map();
        let min = Duration::from_secs(5);
        let stale = Instant::now();

        {
            let mut g = map.lock().unwrap();
            for i in 0..=RATE_LIMIT_MAP_MAX_ENTRIES as i64 {
                g.insert((i, "a".into(), "r".into()), stale);
            }
        }

        // Now insert a new entry well past 2 * min_interval from the
        // stale timestamps. The prune branch keeps only entries newer
        // than `now - 2 * min_interval`, so everything stale should go.
        let now = stale + Duration::from_secs(60);
        assert!(check_and_record_rate_limit_inner(
            &map, 9_999_999, "a", "r", now, min
        ));

        let len = map.lock().unwrap().len();
        // After prune, only the freshly-inserted entry should remain.
        assert_eq!(len, 1, "prune should drop stale entries; got len={}", len);
    }

    #[test]
    fn rate_limit_no_prune_when_within_cap() {
        // Under the cap, pruning must not run even if entries are stale.
        let map = fresh_map();
        let min = Duration::from_secs(5);
        let stale = Instant::now();

        {
            let mut g = map.lock().unwrap();
            // Well under the cap.
            for i in 0..100 {
                g.insert((i, "a".into(), "r".into()), stale);
            }
        }

        let now = stale + Duration::from_secs(60);
        assert!(check_and_record_rate_limit_inner(
            &map, 100_000, "a", "r", now, min
        ));

        // All 100 stale entries plus the new one survive.
        assert_eq!(map.lock().unwrap().len(), 101);
    }
}
