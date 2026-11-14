//! Benchmark reports this agent holds ([`crate::benchmark`]): its own, or
//! imported from whoever measured.
//!
//! A report is kept as the bytes of its signed JSON, with the columns that
//! index it and its visibility, which is this agent's, never part of what
//! the measurer signed: whoever keeps a report cannot change it, only say
//! who sees it. It is read back verified ([`Signed::from_json`]), and only
//! what verifies is ever a report. One id names one report: keeping the
//! same report again only sets its visibility, and another report under an
//! id kept already is refused.
//!
//! **A row that does not read** (a report that no longer verifies, or is
//! another id's, or a visibility that does not parse), as a damaged disk or
//! another program may leave it, hides only itself, as the unreadable task
//! rows of milestone 1 do: the reports a scope sees
//! ([`Store::benchmarks_visible`]) are read by their visibility first and
//! skip it with a warning naming its id, so one bad row never keeps a peer
//! from the others, nor says it exists; the listing of every row
//! ([`Store::benchmarks`]) names it, unreadable, so the operator sees it;
//! asked for by its id ([`Store::benchmark`]), it is an error naming it.

use anyhow::{Context, anyhow, bail, ensure};
use rusqlite::{OptionalExtension, params};
use tracing::warn;

use super::Store;
use crate::benchmark::BenchmarkReport;
use crate::grant::parse_timestamp;
use crate::inventory::{Scope, Visibility, is_project_name};
use crate::signed::Signed;

/// A row of the reports kept: its id, and its report and visibility, or
/// why the row does not read.
#[derive(Debug, Clone, PartialEq)]
pub struct KeptBenchmark {
    pub benchmark_id: String,
    /// The report, verified, and who sees it; or why the row does not
    /// read.
    pub report: Result<(Signed<BenchmarkReport>, Visibility), String>,
}

impl Store {
    /// Keeps `report`, seen by `visibility`, at `now_unix`; or, when this
    /// very report is kept already, sets its visibility.
    ///
    /// An error when another report is kept under its `benchmark_id`, or
    /// when `visibility` names no project or a project that is not a
    /// project name.
    pub fn benchmark_put(
        &self,
        report: &Signed<BenchmarkReport>,
        visibility: &Visibility,
        now_unix: i64,
    ) -> anyhow::Result<()> {
        check_visibility(visibility)?;
        let doc = &report.doc;
        let time = |text: &str| {
            parse_timestamp(text)
                .ok_or_else(|| anyhow!("a report time that does not read: {text:?}"))
        };
        let changed = self
            .conn()
            .execute(
                "INSERT INTO benchmark_reports (benchmark_id, measured_by, measurement_kind,
                     subject, profile, measured_at, expires_at, visibility, report_json,
                     imported_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (benchmark_id) DO UPDATE SET visibility = excluded.visibility
                     WHERE benchmark_reports.report_json = excluded.report_json",
                params![
                    doc.benchmark_id,
                    doc.provenance.measured_by,
                    doc.measurement_kind.as_str(),
                    doc.subject.key(),
                    doc.procedure.profile,
                    time(&doc.measured_at)?,
                    time(&doc.expires_at)?,
                    serde_json::to_string(visibility)?,
                    report.to_json(),
                    now_unix,
                ],
            )
            .with_context(|| format!("cannot keep the benchmark report {}", doc.benchmark_id))?;
        if changed == 0 {
            bail!(
                "another benchmark report is kept as {}: a report id names one report",
                doc.benchmark_id
            );
        }
        Ok(())
    }

    /// The report kept as `id`, verified, and who sees it.
    ///
    /// An error, naming `id`, when its row does not read.
    pub fn benchmark(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<(Signed<BenchmarkReport>, Visibility)>> {
        let row: Option<(String, Vec<u8>)> = self
            .conn()
            .query_row(
                "SELECT visibility, report_json FROM benchmark_reports WHERE benchmark_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((visibility, json)) = row else {
            return Ok(None);
        };
        let visibility = visibility_of(&visibility)
            .map_err(|why| anyhow!("the benchmark report {id} kept does not read: {why}"))?;
        let report = report_of(id, &json)
            .map_err(|why| anyhow!("the benchmark report {id} kept does not read: {why}"))?;
        Ok(Some((report, visibility)))
    }

    /// Every row of the reports kept, by id: each with its report, verified,
    /// and who sees it, or why it does not read.
    pub fn benchmarks(&self) -> anyhow::Result<Vec<KeptBenchmark>> {
        Ok(self
            .benchmark_rows()?
            .into_iter()
            .map(|(benchmark_id, visibility, json)| {
                let report = visibility_of(&visibility)
                    .and_then(|visibility| Ok((report_of(&benchmark_id, &json)?, visibility)));
                KeptBenchmark {
                    benchmark_id,
                    report,
                }
            })
            .collect())
    }

    /// The reports kept whose visibility shows them in `scope`, verified,
    /// by id. A row is read by its visibility first: one the scope does not
    /// see is never verified, and one that does not read is skipped with a
    /// warning naming its id. Whether the scope also shows what a report is
    /// about is the caller's to check
    /// ([`crate::benchmark::Subject::shown_among`]).
    pub fn benchmarks_visible(
        &self,
        scope: &Scope,
    ) -> anyhow::Result<Vec<Signed<BenchmarkReport>>> {
        let mut visible = Vec::new();
        for (benchmark_id, visibility, json) in self.benchmark_rows()? {
            let read = visibility_of(&visibility).and_then(|visibility| {
                if visibility.visible_in(scope) {
                    report_of(&benchmark_id, &json).map(Some)
                } else {
                    Ok(None)
                }
            });
            match read {
                Ok(Some(report)) => visible.push(report),
                Ok(None) => {}
                Err(why) => warn!(
                    %benchmark_id,
                    error = %why,
                    "skipping a benchmark report row that does not read"
                ),
            }
        }
        Ok(visible)
    }

    /// Every row: its id, visibility and report, as kept, by id.
    fn benchmark_rows(&self) -> anyhow::Result<Vec<(String, String, Vec<u8>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT benchmark_id, visibility, report_json FROM benchmark_reports
             ORDER BY benchmark_id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// The visibility a row keeps, or why it does not parse.
fn visibility_of(text: &str) -> Result<Visibility, String> {
    serde_json::from_str(text).map_err(|_| "its visibility does not parse".to_owned())
}

/// The report of the row `id`, verified and of that id, or why not.
fn report_of(id: &str, json: &[u8]) -> Result<Signed<BenchmarkReport>, String> {
    let report = Signed::<BenchmarkReport>::from_json(json)
        .map_err(|err| format!("its report does not verify ({err})"))?;
    if report.doc.benchmark_id != id {
        return Err("its report has another id".to_owned());
    }
    Ok(report)
}

/// A visibility for projects names at least one, and each by a project
/// name.
fn check_visibility(visibility: &Visibility) -> anyhow::Result<()> {
    if let Visibility::Project(projects) | Visibility::Task(projects) = visibility {
        ensure!(
            !projects.is_empty(),
            "a visibility for projects names at least one project"
        );
        ensure!(
            projects.iter().all(|project| is_project_name(project)),
            "a project name is printable ASCII without spaces"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::benchmark::{
        BENCHMARK_SCHEMA, Environment, MeasurementKind, MetricResult, Procedure, Provenance,
        Subject,
    };
    use crate::grant::format_timestamp;
    use crate::identity::AgentKey;

    /// 2026-11-14T00:00:00Z.
    const NOW: i64 = 1_794_614_400;

    fn report(key: &AgentKey, id: &str, value: f64) -> Signed<BenchmarkReport> {
        let doc = BenchmarkReport {
            schema: BENCHMARK_SCHEMA.to_owned(),
            benchmark_id: id.to_owned(),
            subject: Subject::Service("ocr".to_owned()),
            measurement_kind: MeasurementKind::OwnMeasurement,
            procedure: Procedure {
                profile: "service.task".to_owned(),
                profile_version: "1".to_owned(),
                runner: None,
                config: json!({}),
                inputs_hash: None,
            },
            environment: Environment::default(),
            measured_at: format_timestamp(NOW).unwrap(),
            duration_ms: 1000,
            samples: 1,
            expires_at: format_timestamp(NOW + 3600).unwrap(),
            results: vec![MetricResult {
                metric: "total_time".to_owned(),
                unit: "ms".to_owned(),
                statistic: "p50".to_owned(),
                value,
                errors: None,
                uncertainty: None,
            }],
            provenance: Provenance {
                measured_by: key.id(),
                method: "test".to_owned(),
            },
            evidence: None,
        };
        Signed::issue(key, doc).unwrap()
    }

    fn demo() -> Visibility {
        Visibility::Project(vec!["demo".to_owned()])
    }

    #[test]
    fn a_report_is_kept_once_and_its_visibility_beside_it() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        let first = report(&key, "r-1", 10.0);
        assert!(store.benchmark("r-1").unwrap().is_none());
        store
            .benchmark_put(&first, &Visibility::Private, NOW)
            .unwrap();
        assert_eq!(
            store.benchmark("r-1").unwrap(),
            Some((first.clone(), Visibility::Private))
        );
        // The same report again: only who sees it changes.
        store.benchmark_put(&first, &demo(), NOW + 1).unwrap();
        assert_eq!(
            store.benchmark("r-1").unwrap(),
            Some((first.clone(), demo()))
        );
        // Another report under its id: refused, and nothing changes.
        let other = report(&key, "r-1", 20.0);
        let err = store
            .benchmark_put(&other, &Visibility::Public, NOW)
            .unwrap_err();
        assert!(
            err.to_string().contains("another benchmark report"),
            "{err}"
        );
        assert_eq!(store.benchmark("r-1").unwrap(), Some((first, demo())));
        // A visibility for no project, or one that is not a project name.
        for bad in [
            Visibility::Project(Vec::new()),
            Visibility::Task(vec!["two words".to_owned()]),
        ] {
            assert!(
                store
                    .benchmark_put(&report(&key, "r-2", 1.0), &bad, NOW)
                    .is_err()
            );
        }
        assert!(store.benchmark("r-2").unwrap().is_none());
    }

    #[test]
    fn reports_visible_in_a_scope_are_those_their_visibility_shows() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        let kept = [
            ("r-private", Visibility::Private),
            ("r-public", Visibility::Public),
            ("r-demo", demo()),
            ("r-task", Visibility::Task(vec!["demo".to_owned()])),
        ];
        for (id, visibility) in &kept {
            store
                .benchmark_put(&report(&key, id, 1.0), visibility, NOW)
                .unwrap();
        }
        let ids = |scope: &Scope| -> Vec<String> {
            store
                .benchmarks_visible(scope)
                .unwrap()
                .into_iter()
                .map(|report| report.doc.benchmark_id)
                .collect()
        };
        assert_eq!(ids(&Scope::Public), ["r-public"]);
        assert_eq!(
            ids(&Scope::Project("demo".to_owned())),
            ["r-demo", "r-public"]
        );
        assert_eq!(
            ids(&Scope::Task("demo".to_owned())),
            ["r-demo", "r-public", "r-task"]
        );
        assert_eq!(ids(&Scope::Task("other".to_owned())), ["r-public"]);
        assert_eq!(store.benchmarks().unwrap().len(), 4);
    }

    #[test]
    fn a_row_that_does_not_read_hides_only_itself() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        for id in ["r-1", "r-2", "r-3", "r-4"] {
            store
                .benchmark_put(&report(&key, id, 10.0), &Visibility::Public, NOW)
                .unwrap();
        }
        let tampered = String::from_utf8(report(&key, "r-1", 10.0).to_json())
            .unwrap()
            .replace("\"value\":10", "\"value\":1");
        let conn = store.conn();
        let set = |column: &str, value: &dyn rusqlite::ToSql, id: &str| {
            let sql = format!("UPDATE benchmark_reports SET {column} = ?1 WHERE benchmark_id = ?2");
            assert_eq!(conn.execute(&sql, rusqlite::params![value, id]).unwrap(), 1);
        };
        set("report_json", &tampered.as_bytes(), "r-1");
        set("visibility", &"everyone", "r-2");
        set("report_json", &report(&key, "r-9", 10.0).to_json(), "r-3");
        drop(conn);

        // Those a scope sees: the row that reads.
        let visible: Vec<String> = store
            .benchmarks_visible(&Scope::Public)
            .unwrap()
            .into_iter()
            .map(|report| report.doc.benchmark_id)
            .collect();
        assert_eq!(visible, ["r-4"]);
        // Every row, those that do not read named so.
        let rows: Vec<(String, Result<(), String>)> = store
            .benchmarks()
            .unwrap()
            .into_iter()
            .map(|kept| (kept.benchmark_id, kept.report.map(|_| ())))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "r-1".to_owned(),
                    Err("its report does not verify (bad signature)".to_owned())
                ),
                (
                    "r-2".to_owned(),
                    Err("its visibility does not parse".to_owned())
                ),
                (
                    "r-3".to_owned(),
                    Err("its report has another id".to_owned())
                ),
                ("r-4".to_owned(), Ok(())),
            ]
        );
        // Asked for by its id: an error naming it.
        for id in ["r-1", "r-2", "r-3"] {
            let err = store.benchmark(id).unwrap_err();
            assert!(format!("{err:#}").contains(id), "{err:#}");
        }
        assert!(store.benchmark("r-4").unwrap().is_some());
    }
}
