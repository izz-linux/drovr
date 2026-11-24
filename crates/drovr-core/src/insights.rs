//! What the Insights view shows, added up from the spend log.

use crate::engine::Engine;
use crate::spend::Row;
use anyhow::{ensure, Result};
use serde::Serialize;
use std::collections::BTreeMap;

/// Runs and what they cost.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Total {
    pub cost: f64,
    pub runs: u64,
    /// Runs whose CLI reported no cost; `cost` leaves them out.
    pub unpriced: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Part {
    pub name: String,
    #[serde(flatten)]
    pub total: Total,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Week {
    pub start: f64,
    #[serde(flatten)]
    pub total: Total,
    pub by_harness: Vec<Part>,
    /// When each retro that started this week did.
    pub retros: Vec<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Insights {
    pub weeks: Vec<Week>,
    pub total: Total,
    /// Each of these adds up to `total`, most expensive first.
    pub by_stage: Vec<Part>,
    pub by_harness: Vec<Part>,
    /// `(OC)` marks a model run through OpenCode.
    pub by_model: Vec<Part>,
    /// A run on several tickets is split evenly between them.
    pub by_ticket: Vec<Part>,
    /// A full week's average, from the first week with a run; None until
    /// there's been one.
    pub week_average: Option<f64>,
    /// What a retro's agent cost, over the retros with a cost logged (those
    /// from before the spend log have none); None without one.
    pub retro_average: Option<f64>,
    /// 0 for none.
    pub weekly_target_usd: f64,
}

impl Total {
    /// Add a row, as one of `share` that split it. A run's other models
    /// don't count as runs.
    fn add(&mut self, row: &Row, share: usize) {
        self.cost += row.cost_usd.unwrap_or(0.0) / share as f64;
        if row.main {
            self.runs += 1;
            self.unpriced += u64::from(row.cost_usd.is_none());
        }
    }
}

type Parts = BTreeMap<String, Total>;

fn add(to: &mut Parts, name: &str, row: &Row, share: usize) {
    to.entry(name.to_string()).or_default().add(row, share);
}

fn or<'a>(name: &'a str, none: &'a str) -> &'a str {
    if name.is_empty() {
        none
    } else {
        name
    }
}

/// The model a row is listed under: Claude Code and an AI gateway in OpenCode
/// run the same models.
pub(crate) fn model_label(row: &Row) -> String {
    let via = match row.harness.as_str() {
        "opencode" => " (OC)",
        _ => "",
    };
    format!("{}{via}", or(&row.model, "unknown"))
}

/// Most expensive first, then by name.
fn sorted(parts: Parts) -> Vec<Part> {
    let mut out: Vec<Part> = parts
        .into_iter()
        .map(|(name, total)| Part { name, total })
        .collect();
    out.sort_by(|a, b| b.total.cost.total_cmp(&a.total.cost));
    out
}

impl Engine {
    /// Add up the spend log by week. `bounds` are the starts of the weeks
    /// and the end of the last, which is still going.
    pub fn insights(&self, bounds: &[f64]) -> Result<Insights> {
        ensure!(
            bounds.len() >= 2 && bounds.windows(2).all(|w| w[0] < w[1]),
            "the weeks must be in order"
        );
        let retros = self.retros();
        let week_of = |ts: f64| {
            let next = bounds.partition_point(|b| *b <= ts);
            (next > 0 && next < bounds.len()).then(|| next - 1)
        };
        let mut weeks: Vec<Week> = bounds[..bounds.len() - 1]
            .iter()
            .map(|start| Week {
                start: *start,
                ..Week::default()
            })
            .collect();
        for r in &retros {
            if let Some(w) = week_of(r.started) {
                weeks[w].retros.push(r.started);
            }
        }
        for w in &mut weeks {
            w.retros.sort_by(f64::total_cmp);
        }

        let mut out = Insights {
            weekly_target_usd: self.cfg.insights.weekly_target_usd,
            ..Insights::default()
        };
        let (mut stages, mut harnesses, mut models, mut tickets) =
            (Parts::new(), Parts::new(), Parts::new(), Parts::new());
        let mut weekly: Vec<Parts> = vec![Parts::new(); weeks.len()];
        let mut retro_costs: BTreeMap<&str, f64> = BTreeMap::new();
        for row in self.store.spend_since(bounds[0])? {
            let Some(w) = week_of(row.ts) else { continue };
            out.total.add(&row, 1);
            weeks[w].total.add(&row, 1);
            let harness = or(&row.harness, "unknown");
            add(&mut weekly[w], harness, &row, 1);
            add(&mut harnesses, harness, &row, 1);
            add(&mut stages, or(&row.stage, "no stage"), &row, 1);
            add(&mut models, &model_label(&row), &row, 1);
            let on: Vec<&str> = row.tickets.split(',').collect();
            for ticket in &on {
                add(&mut tickets, or(ticket, "no ticket"), &row, on.len());
            }
            let retro = retros
                .iter()
                .filter(|r| r.agent == row.agent && r.started <= row.ts)
                .max_by(|a, b| a.started.total_cmp(&b.started));
            let retro = retro.filter(|r| week_of(r.started).is_some());
            if let (Some(r), Some(usd)) = (retro, row.cost_usd) {
                *retro_costs.entry(&r.dir).or_default() += usd;
            }
        }
        for (week, parts) in weeks.iter_mut().zip(weekly) {
            week.by_harness = sorted(parts);
        }
        let first = weeks.iter().position(|w| w.total.runs > 0);
        let full = &weeks[first.unwrap_or(weeks.len() - 1).min(weeks.len() - 1)..weeks.len() - 1];
        if !full.is_empty() {
            let spent: f64 = full.iter().map(|w| w.total.cost).sum();
            out.week_average = Some(spent / full.len() as f64);
        }
        if !retro_costs.is_empty() {
            let spent: f64 = retro_costs.values().sum();
            out.retro_average = Some(spent / retro_costs.len() as f64);
        }
        out.by_stage = sorted(stages);
        out.by_harness = sorted(harnesses);
        out.by_model = sorted(models);
        out.by_ticket = sorted(tickets);
        out.weeks = weeks;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;
    use serde_json::json;
    use std::path::PathBuf;

    fn engine(case: &str, rows: &[Row]) -> (Engine, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("drovr-insights-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = dir.join("plans").display().to_string();
        cfg.insights.weekly_target_usd = 50.0;
        let store = Store::open(&dir.join("t.db")).unwrap();
        store.add_spend(rows).unwrap();
        (Engine::with(store, cfg, "drovr".into()), dir)
    }

    fn row(ts: f64, harness: &str, model: &str, stage: &str, cost: Option<f64>) -> Row {
        Row {
            ts,
            agent: "b".into(),
            harness: harness.into(),
            model: model.into(),
            stage: stage.into(),
            main: true,
            cost_usd: cost,
            key: format!("{ts}{harness}{model}"),
            ..Row::default()
        }
    }

    fn parts(of: &[Part]) -> Vec<(&str, f64, u64, u64)> {
        of.iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.total.cost,
                    p.total.runs,
                    p.total.unpriced,
                )
            })
            .collect()
    }

    const WEEKS: [f64; 5] = [100.0, 200.0, 300.0, 400.0, 500.0];

    #[test]
    fn runs_land_in_their_week() {
        let (e, dir) = engine(
            "weeks",
            &[
                row(99.0, "claude", "early", "", Some(9.0)),
                row(200.0, "claude", "m", "", Some(1.0)),
                row(299.9, "opencode", "m", "", Some(2.0)),
                row(450.0, "claude", "m", "", Some(8.0)),
                row(500.0, "claude", "late", "", Some(9.0)),
            ],
        );
        let i = e.insights(&WEEKS).unwrap();
        let weeks: Vec<_> = i.weeks.iter().map(|w| (w.start, w.total.cost)).collect();
        assert_eq!(
            weeks,
            [(100.0, 0.0), (200.0, 3.0), (300.0, 0.0), (400.0, 8.0)]
        );
        assert_eq!(
            parts(&i.weeks[1].by_harness),
            [("opencode", 2.0, 1, 0), ("claude", 1.0, 1, 0)]
        );
        assert_eq!((i.total.cost, i.total.runs), (11.0, 3));
        // Weeks 2 and 3: nothing before the first run, nor the one going.
        assert_eq!(i.week_average, Some(1.5));
        assert_eq!(i.weekly_target_usd, 50.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_breakdown_adds_up() {
        let mut helper = row(110.0, "claude", "haiku", "build", Some(0.5));
        helper.main = false;
        let mut shared = row(110.0, "claude", "opus", "build", Some(4.0));
        shared.tickets = "SRE-1,SRE-2".into();
        helper.tickets = shared.tickets.clone();
        let mut codex = row(120.0, "codex", "gpt", "", None);
        codex.tickets = "SRE-1".into();
        let (e, dir) = engine(
            "parts",
            &[shared, helper, codex, row(130.0, "", "x", "", Some(1.0))],
        );
        let i = e.insights(&WEEKS).unwrap();
        assert_eq!((i.total.cost, i.total.runs, i.total.unpriced), (5.5, 3, 1));
        for (what, of) in [
            ("stage", &i.by_stage),
            ("harness", &i.by_harness),
            ("model", &i.by_model),
            ("ticket", &i.by_ticket),
        ] {
            let sum: f64 = of.iter().map(|p| p.total.cost).sum();
            assert!((sum - 5.5).abs() < 1e-9, "{what}: {sum}");
        }
        assert_eq!(
            parts(&i.by_model),
            [
                ("opus", 4.0, 1, 0),
                ("x", 1.0, 1, 0),
                ("haiku", 0.5, 0, 0),
                ("gpt", 0.0, 1, 1)
            ]
        );
        assert_eq!(
            parts(&i.by_harness),
            [
                ("claude", 4.5, 1, 0),
                ("unknown", 1.0, 1, 0),
                ("codex", 0.0, 1, 1)
            ]
        );
        assert_eq!(
            parts(&i.by_stage),
            [("build", 4.5, 1, 0), ("no stage", 1.0, 2, 1)]
        );
        assert_eq!(
            parts(&i.by_ticket),
            [
                ("SRE-1", 2.25, 2, 1),
                ("SRE-2", 2.25, 1, 0),
                ("no ticket", 1.0, 1, 0)
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_model_through_opencode_is_marked() {
        let (e, dir) = engine(
            "oc",
            &[
                row(110.0, "claude", "claude-opus-5-5", "", Some(3.0)),
                row(111.0, "opencode", "claude-opus-5-5", "", Some(2.0)),
                row(112.0, "opencode", "", "", Some(1.0)),
            ],
        );
        let i = e.insights(&WEEKS).unwrap();
        assert_eq!(
            parts(&i.by_model),
            [
                ("claude-opus-5-5", 3.0, 1, 0),
                ("claude-opus-5-5 (OC)", 2.0, 1, 0),
                ("unknown (OC)", 1.0, 1, 0),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retros_mark_their_week_and_are_averaged() {
        let mut spent = row(250.0, "claude", "m", "", Some(6.0));
        spent.agent = "r".into();
        // Before the retro that agent ran: not its cost.
        let mut before = row(150.0, "claude", "m", "", Some(5.0));
        before.agent = "r2".into();
        let (e, dir) = engine(
            "retros",
            &[spent, before, row(250.0, "codex", "m", "", Some(1.0))],
        );
        for (name, agent, started) in [("a", "r", 200.0), ("b", "r2", 399.0), ("old", "r", 50.0)] {
            let d = dir.join("plans/retros").join(name);
            std::fs::create_dir_all(&d).unwrap();
            let meta =
                json!({ "tickets": ["SRE-1"], "started": started, "agent": agent, "note": "" });
            std::fs::write(d.join("retro.json"), meta.to_string()).unwrap();
        }
        let i = e.insights(&WEEKS).unwrap();
        let marks: Vec<_> = i.weeks.iter().map(|w| w.retros.clone()).collect();
        assert_eq!(marks, [vec![], vec![200.0], vec![399.0], vec![]]);
        // The second has no run logged since it started.
        assert_eq!(i.retro_average, Some(6.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_recorded_is_empty_weeks() {
        let (e, dir) = engine("empty", &[]);
        let i = e.insights(&WEEKS).unwrap();
        assert_eq!(i.weeks.len(), 4);
        assert_eq!(i.total, Total::default());
        assert_eq!((i.week_average, i.retro_average), (None, None));
        assert!(i.by_model.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn weeks_out_of_order_are_refused() {
        let (e, dir) = engine("order", &[]);
        for bad in [&[][..], &[1.0], &[2.0, 1.0], &[1.0, 1.0], &[1.0, f64::NAN]] {
            assert!(e.insights(bad).is_err(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_retro_with_no_cost_logged_isnt_averaged() {
        for (case, rows, want) in [
            ("none", vec![], None),
            (
                "unpriced",
                vec![row(15.0, "codex", "m", "retro", None)],
                None,
            ),
            (
                "one",
                vec![row(15.0, "claude", "m", "retro", Some(4.0))],
                Some(4.0),
            ),
        ] {
            let (e, dir) = engine(&format!("retro-{case}"), &rows);
            for (name, agent, started) in [("a", "old", 11.0), ("b", "b", 12.0)] {
                let d = dir.join("plans/retros").join(name);
                std::fs::create_dir_all(&d).unwrap();
                let meta = json!({ "tickets": ["SRE-1"], "agent": agent, "started": started });
                std::fs::write(d.join("retro.json"), meta.to_string()).unwrap();
            }
            let i = e.insights(&[10.0, 20.0]).unwrap();
            assert_eq!(i.weeks[0].retros, [11.0, 12.0], "{case}");
            assert_eq!(i.retro_average, want, "{case}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
