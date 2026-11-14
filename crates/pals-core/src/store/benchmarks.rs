//! Benchmark reports this agent holds ([`crate::benchmark`]): its own, or
//! imported from whoever measured.
//!
//! A report is kept as the bytes of its signed JSON, with the columns that
//! index it and its visibility, which is this agent's, never part of what
//! the measurer signed: whoever keeps a report cannot change it, only say
//! who sees it. It is read back verified ([`Signed::from_json`]): a row
//! that no longer verifies is an error, never a report. One id names one
//! report: keeping the same report again only sets its visibility, and
//! another report under an id kept already is refused.

use anyhow::{Context, anyhow, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::benchmark::BenchmarkReport;
use crate::grant::parse_timestamp;
use crate::inventory::{Scope, Visibility, is_project_name};
use crate::signed::Signed;

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
    pub fn benchmark(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<(Signed<BenchmarkReport>, Visibility)>> {
        let row: Option<(Vec<u8>, String)> = self
            .conn()
            .query_row(
                "SELECT report_json, visibility FROM benchmark_reports WHERE benchmark_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(json, visibility)| read(&json, &visibility))
            .transpose()
    }

    /// Every report kept, verified, with who sees it, by id.
    pub fn benchmarks(&self) -> anyhow::Result<Vec<(Signed<BenchmarkReport>, Visibility)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT report_json, visibility FROM benchmark_reports ORDER BY benchmark_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (json, visibility) = row?;
            read(&json, &visibility)
        })
        .collect()
    }

    /// The reports kept whose visibility shows them in `scope`, verified,
    /// by id. Whether the scope also shows what a report is about is the
    /// caller's to check ([`crate::benchmark::Subject::shown_among`]).
    pub fn benchmarks_visible(
        &self,
        scope: &Scope,
    ) -> anyhow::Result<Vec<Signed<BenchmarkReport>>> {
        Ok(self
            .benchmarks()?
            .into_iter()
            .filter(|(_, visibility)| visibility.visible_in(scope))
            .map(|(report, _)| report)
            .collect())
    }
}

/// The report of a row, verified, and its visibility.
fn read(json: &[u8], visibility: &str) -> anyhow::Result<(Signed<BenchmarkReport>, Visibility)> {
    let report = Signed::<BenchmarkReport>::from_json(json)
        .map_err(|err| anyhow!("a benchmark report kept that does not verify: {err}"))?;
    let visibility = serde_json::from_str(visibility).with_context(|| {
        format!(
            "the visibility of {} does not read",
            report.doc.benchmark_id
        )
    })?;
    Ok((report, visibility))
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
    fn a_kept_report_that_no_longer_verifies_is_an_error() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        store
            .benchmark_put(&report(&key, "r-1", 10.0), &Visibility::Public, NOW)
            .unwrap();
        let tampered = String::from_utf8(report(&key, "r-1", 10.0).to_json())
            .unwrap()
            .replace("\"value\":10", "\"value\":1");
        store
            .conn()
            .execute(
                "UPDATE benchmark_reports SET report_json = ?1",
                [tampered.as_bytes()],
            )
            .unwrap();
        assert!(store.benchmark("r-1").is_err());
        assert!(store.benchmarks_visible(&Scope::Public).is_err());
    }
}
