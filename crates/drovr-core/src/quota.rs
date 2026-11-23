//! GitHub's API quota, read before each poll so drovr slows down as it runs
//! low. `gh pr view` spends the GraphQL quota and `gh search prs` the search
//! one; the REST quota isn't drovr's to spend, so it's ignored.

use crate::shell::{self, quote};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// When the next automatic poll may run (unix seconds; 0 is no hold), and
/// whether the last one was skipped because of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hold {
    pub until: f64,
    pub paused: bool,
}

/// Below this share of the GraphQL quota, polls run `SLOWDOWN` times less often.
const LOW: f64 = 0.25;
/// Below this share of a quota, only a forced poll runs before its reset.
const NEARLY_OUT: f64 = 0.05;
const SLOWDOWN: f64 = 4.0;

/// Whether to poll now, and the hold that leaves. Reading the quota doesn't
/// count against it; when it can't be read (some servers have no limits),
/// polling goes on as usual.
pub fn gate(gh: &str, last: f64, every: f64, force: bool) -> (bool, Hold) {
    match read(gh) {
        Ok(v) => decide(&v["resources"], crate::store::now(), last, every, force),
        Err(e) => {
            eprintln!("drovr: couldn't read GitHub's API quota: {e:#}");
            (true, Hold::default())
        }
    }
}

fn read(gh: &str) -> Result<Value> {
    let cmd = format!("{} api rate_limit", quote(gh));
    let out = shell::sh(&cmd, None, None, Duration::from_secs(60))?.into_stdout(&cmd)?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("`{cmd}` didn't print JSON: {e}"))
}

fn decide(resources: &Value, now: f64, last: f64, every: f64, force: bool) -> (bool, Hold) {
    let mut paused_until: f64 = 0.0;
    let mut empty = false;
    let mut slow_until: f64 = 0.0;
    for name in ["graphql", "search"] {
        let r = &resources[name];
        let (Some(limit), Some(left), Some(reset)) = (
            r["limit"].as_f64(),
            r["remaining"].as_f64(),
            r["reset"].as_f64(),
        ) else {
            continue;
        };
        if reset <= now {
            continue;
        }
        if left < limit * NEARLY_OUT {
            paused_until = paused_until.max(reset);
            empty |= left == 0.0;
        } else if name == "graphql" && left < limit * LOW {
            // Search refills every minute, so it's only ever paused.
            slow_until = reset;
        }
    }
    let hold = |until, paused| Hold { until, paused };
    if paused_until > 0.0 {
        let poll = force && !empty;
        return (poll, hold(paused_until, !poll));
    }
    if slow_until > 0.0 {
        let due = last + every * SLOWDOWN;
        if force || now >= due {
            return (true, hold((now + every * SLOWDOWN).min(slow_until), false));
        }
        return (false, hold(due.min(slow_until), true));
    }
    (true, Hold::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: f64 = 10_000.0;

    fn resources(core: u64, graphql: u64, search: u64) -> Value {
        json!({
            "core": { "limit": 5000, "remaining": core, "reset": 13_000 },
            "graphql": { "limit": 5000, "remaining": graphql, "reset": 12_000 },
            "search": { "limit": 30, "remaining": search, "reset": 10_040 },
        })
    }

    #[test]
    fn paces_polls_by_what_is_left() {
        // (case, resources, last, force) -> (poll, until, paused), every 120s
        let cases = [
            (
                "plenty",
                resources(5000, 5000, 30),
                9_880.0,
                false,
                (true, 0.0, false),
            ),
            (
                "REST empty is ignored",
                resources(0, 4967, 29),
                9_880.0,
                false,
                (true, 0.0, false),
            ),
            (
                "exactly a quarter",
                resources(1, 1250, 30),
                9_880.0,
                false,
                (true, 0.0, false),
            ),
            (
                "low, not yet 4x",
                resources(1, 1249, 30),
                9_880.0,
                false,
                (false, 10_360.0, true),
            ),
            (
                "low, 4x passed",
                resources(1, 1249, 30),
                9_520.0,
                false,
                (true, 10_480.0, false),
            ),
            (
                "low, forced",
                resources(1, 1249, 30),
                9_999.0,
                true,
                (true, 10_480.0, false),
            ),
            (
                "low, first poll",
                resources(1, 1249, 30),
                0.0,
                false,
                (true, 10_480.0, false),
            ),
            (
                "nearly out",
                resources(1, 249, 30),
                0.0,
                false,
                (false, 12_000.0, true),
            ),
            (
                "nearly out, forced",
                resources(1, 249, 30),
                0.0,
                true,
                (true, 12_000.0, false),
            ),
            (
                "empty, forced",
                resources(1, 0, 30),
                0.0,
                true,
                (false, 12_000.0, true),
            ),
            (
                "search low is fine",
                resources(1, 5000, 5),
                9_880.0,
                false,
                (true, 0.0, false),
            ),
            (
                "search empty",
                resources(1, 5000, 0),
                9_880.0,
                false,
                (false, 10_040.0, true),
            ),
            (
                "both out waits for the later",
                resources(1, 0, 0),
                0.0,
                false,
                (false, 12_000.0, true),
            ),
            ("no quotas", json!({}), 9_880.0, false, (true, 0.0, false)),
        ];
        for (name, r, last, force, (poll, until, paused)) in cases {
            assert_eq!(
                decide(&r, NOW, last, 120.0, force),
                (poll, Hold { until, paused }),
                "{name}"
            );
        }
    }

    #[test]
    fn a_slow_hold_ends_at_the_reset() {
        let r = json!({ "graphql": { "limit": 5000, "remaining": 1000, "reset": 10_100 } });
        assert_eq!(decide(&r, NOW, 9_000.0, 120.0, false).1.until, 10_100.0);
    }

    #[test]
    fn a_quota_past_its_reset_holds_nothing() {
        let r = json!({ "graphql": { "limit": 5000, "remaining": 0, "reset": 9_999 } });
        assert_eq!(
            decide(&r, NOW, 9_000.0, 120.0, false),
            (true, Hold::default())
        );
    }

    #[test]
    fn an_unreadable_quota_does_not_stop_polling() {
        for gh in ["false", "true", "echo nope #"] {
            assert_eq!(gate(gh, 0.0, 120.0, false), (true, Hold::default()), "{gh}");
        }
    }
}
