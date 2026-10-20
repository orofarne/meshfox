//! Persistent, invocation-scoped transcripts, independent of per-block retention.
use crate::{run_ledger::RunLedger, session_db::sqlite_err};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct RunJournal {
    conn: Arc<Mutex<Connection>>,
    max_bytes: usize,
    keep: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: i64,
    pub live_id: String,
    pub node_id: String,
    pub block: String,
    pub path: Vec<String>,
    pub source: String,
    pub options: Value,
    pub status: String,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub truncated: bool,
    pub output_disabled: bool,
    pub incomplete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub seq: i64,
    pub time: String,
    pub event: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub runs: Vec<Summary>,
    pub selected: Option<Summary>,
    pub events: Vec<Event>,
    pub next_seq: i64,
    pub more: bool,
}

impl RunJournal {
    pub fn new(conn: Arc<Mutex<Connection>>, max_bytes: usize, keep: usize) -> io::Result<Self> {
        conn.lock().unwrap().execute_batch("\
            CREATE TABLE IF NOT EXISTS chain_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, live_id TEXT NOT NULL,
                node_id TEXT NOT NULL, block TEXT NOT NULL, path TEXT NOT NULL,
                source TEXT NOT NULL, options TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running', exit_code INTEGER,
                started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                ended_at TEXT, truncated INTEGER NOT NULL DEFAULT 0,
                incomplete INTEGER NOT NULL DEFAULT 0,
                output_disabled INTEGER NOT NULL, output_bytes INTEGER NOT NULL DEFAULT 0,
                next_seq INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS chain_events (
                chain_id INTEGER NOT NULL, seq INTEGER NOT NULL,
                time TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                node_id TEXT, block TEXT, event TEXT NOT NULL,
                PRIMARY KEY(chain_id, seq)
            );
            CREATE TABLE IF NOT EXISTS chain_steps (
                chain_id INTEGER NOT NULL,node_id TEXT NOT NULL,block TEXT NOT NULL,
                PRIMARY KEY(chain_id,node_id,block)
            );
            CREATE INDEX IF NOT EXISTS chain_events_address ON chain_events(node_id, block, chain_id);
            UPDATE chain_runs SET status='interrupted', ended_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                WHERE status='running';
        ").map_err(sqlite_err)?;
        let _ = conn.lock().unwrap().execute(
            "ALTER TABLE chain_runs ADD COLUMN incomplete INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let journal = Self {
            conn,
            max_bytes,
            keep: keep.max(1),
        };
        journal.prune()?;
        Ok(journal)
    }

    pub fn start(
        &self,
        live_id: &str,
        node: &str,
        block: &str,
        path: &[String],
        source: &str,
        options: Value,
    ) -> io::Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sqlite_err)?;
        tx.execute(
            "INSERT INTO chain_runs(live_id,node_id,block,path,source,options,output_disabled)
            VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                live_id,
                node,
                block,
                serde_json::to_string(path).map_err(io::Error::other)?,
                source,
                options.to_string(),
                self.max_bytes == 0
            ],
        )
        .map_err(sqlite_err)?;
        let id = tx.last_insert_rowid();
        if let Some(steps) = options["steps"].as_array() {
            for step in steps {
                if let (Some(node), Some(block)) = (step["nodeId"].as_str(), step["block"].as_str())
                {
                    tx.execute("INSERT OR IGNORE INTO chain_steps(chain_id,node_id,block) VALUES(?1,?2,?3)",params![id,node,block]).map_err(sqlite_err)?;
                }
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO chain_steps(chain_id,node_id,block) VALUES(?1,?2,?3)",
            params![id, node, block],
        )
        .map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)?;
        Ok(id)
    }

    fn unreached(&self, id: i64) -> io::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt=conn.prepare("SELECT s.node_id,s.block FROM chain_steps s WHERE s.chain_id=?1
            AND NOT EXISTS(SELECT 1 FROM chain_events e WHERE e.chain_id=s.chain_id AND e.node_id=s.node_id AND e.block=s.block) ORDER BY s.rowid").map_err(sqlite_err)?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    pub fn append(&self, id: i64, mut event: Value, ledger: &RunLedger) -> io::Result<()> {
        if matches!(event["type"].as_str(), Some("done" | "killed")) {
            for (node, block) in self.unreached(id)? {
                self.append(id,json!({"type":"step-not-started","nodeId":node,"block":block,"reason":"chain ended before this step"}),ledger)?;
            }
        }
        // Existing live clients still receive cached output on skipped events.
        // The transcript records a skip plus immutable provenance instead.
        if event["type"] == "step-skipped" {
            event.as_object_mut().unwrap().remove("output");
            event["reason"] = json!("already fresh");
            let source = event["sourceRunId"].as_i64();
            if let Some(source) = source {
                let node = event["nodeId"].as_str().unwrap_or("");
                let block = event["block"].as_str().unwrap_or("");
                let summary = ledger.get_run(source, node, block, None)?;
                let output = ledger.load_output(source)?;
                event["effective"] = json!({
                    "runId": source,
                    "startedAt": summary.as_ref().map(|r| &r.started_at),
                    "exitCode": summary.as_ref().and_then(|r| r.exit_code),
                    "durationMs": summary.as_ref().and_then(|r| r.duration_ms),
                    "available": output.is_some(),
                    "lines": output.unwrap_or_default().into_iter().map(|(stream,text)| json!({"stream":stream,"text":text})).collect::<Vec<_>>()
                });
            } else {
                event["effective"] = json!({"available":false,"lines":[]});
            }
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sqlite_err)?;
        let (seq, used): (i64, usize) = tx
            .query_row(
                "SELECT next_seq,output_bytes FROM chain_runs WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite_err)?;
        let mut remaining = self.max_bytes.saturating_sub(used);
        let before = remaining;
        let mut truncated = false;
        if event["type"] == "output" {
            trim_text(&mut event["text"], &mut remaining, &mut truncated);
        }
        if let Some(lines) = event
            .pointer_mut("/effective/lines")
            .and_then(Value::as_array_mut)
        {
            for line in lines.iter_mut() {
                trim_text(&mut line["text"], &mut remaining, &mut truncated);
            }
            lines.retain(|line| line["text"].as_str().is_some_and(|text| !text.is_empty()));
        }
        if truncated {
            event["truncated"] = json!(true);
        }
        let terminal = match event["type"].as_str() {
            Some("done") => Some((
                if event["exitCode"] == 0 {
                    "succeeded"
                } else {
                    "failed"
                },
                event["exitCode"].as_i64(),
            )),
            Some("killed") => Some(("killed", None)),
            _ => None,
        };
        if let (Some(node), Some(block)) = (event["nodeId"].as_str(), event["block"].as_str()) {
            tx.execute(
                "INSERT OR IGNORE INTO chain_steps(chain_id,node_id,block) VALUES(?1,?2,?3)",
                params![id, node, block],
            )
            .map_err(sqlite_err)?;
        }
        // Metadata stays available even when storing output is disabled.
        let omit = event["type"] == "output" && event["text"].as_str() == Some("") && truncated;
        if !omit {
            tx.execute(
                "INSERT INTO chain_events(chain_id,seq,node_id,block,event) VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    seq,
                    event["nodeId"].as_str(),
                    event["block"].as_str(),
                    event.to_string()
                ],
            )
            .map_err(sqlite_err)?;
        }
        tx.execute("UPDATE chain_runs SET next_seq=next_seq+1,output_bytes=output_bytes+?2,truncated=MAX(truncated,?3) WHERE id=?1",
            params![id,before-remaining,truncated && self.max_bytes > 0]).map_err(sqlite_err)?;
        if let Some((mut status, mut code)) = terminal {
            let errors:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM chain_events WHERE chain_id=?1 AND json_extract(event,'$.type')='error')",[id],|r|r.get(0)).map_err(sqlite_err)?;
            if status == "succeeded" && errors {
                status = "failed";
                code = Some(1);
            }
            tx.execute("UPDATE chain_runs SET status=?2,exit_code=?3,ended_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",params![id,status,code]).map_err(sqlite_err)?;
        }
        tx.commit().map_err(sqlite_err)?;
        drop(conn);
        if terminal.is_some() {
            self.prune()?;
        }
        Ok(())
    }

    // Output is flushed in short bounded batches, avoiding an fsync per line.
    // Control events still commit immediately, so step/terminal boundaries are durable.
    fn append_outputs(&self, id: i64, events: &[Value]) -> io::Result<()> {
        if events.is_empty() || self.max_bytes == 0 {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sqlite_err)?;
        let (mut seq, used): (i64, usize) = tx
            .query_row(
                "SELECT next_seq,output_bytes FROM chain_runs WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite_err)?;
        let mut remaining = self.max_bytes.saturating_sub(used);
        let before = remaining;
        let mut truncated = false;
        for event in events {
            let mut event = event.clone();
            trim_text(&mut event["text"], &mut remaining, &mut truncated);
            if !event["text"].as_str().unwrap_or("").is_empty() || !truncated {
                tx.execute("INSERT INTO chain_events(chain_id,seq,node_id,block,event) VALUES(?1,?2,?3,?4,?5)",params![id,seq,event["nodeId"].as_str(),event["block"].as_str(),event.to_string()]).map_err(sqlite_err)?;
            }
            seq += 1;
        }
        tx.execute("UPDATE chain_runs SET next_seq=?2,output_bytes=output_bytes+?3,truncated=MAX(truncated,?4) WHERE id=?1",params![id,seq,before-remaining,truncated]).map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)
    }

    fn mark_incomplete(&self, id: i64) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE chain_runs SET incomplete=1 WHERE id=?1", [id])
            .map_err(sqlite_err)?;
        Ok(())
    }

    pub fn interrupt(&self, id: i64) -> io::Result<()> {
        self.conn.lock().unwrap().execute("UPDATE chain_runs SET status='interrupted',ended_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND status='running'",[id]).map_err(sqlite_err)?;
        self.prune()
    }

    fn prune(&self) -> io::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sqlite_err)?;
        tx.execute("DELETE FROM chain_steps WHERE chain_id IN (SELECT id FROM chain_runs WHERE status!='running' ORDER BY id DESC LIMIT -1 OFFSET ?1)",[self.keep]).map_err(sqlite_err)?;
        tx.execute("DELETE FROM chain_events WHERE chain_id IN (SELECT id FROM chain_runs WHERE status!='running' ORDER BY id DESC LIMIT -1 OFFSET ?1)",[self.keep]).map_err(sqlite_err)?;
        tx.execute("DELETE FROM chain_runs WHERE id IN (SELECT id FROM chain_runs WHERE status!='running' ORDER BY id DESC LIMIT -1 OFFSET ?1)",[self.keep]).map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)
    }

    pub fn list(&self, address: Option<(&str, &str)>, chain: bool) -> io::Result<Vec<Summary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id,live_id,node_id,block,path,source,options,status,exit_code,started_at,ended_at,truncated,output_disabled,incomplete FROM chain_runs c
            WHERE ?1 IS NULL OR (?3 AND c.node_id=?1 AND c.block=?2)
              OR (NOT ?3 AND EXISTS(SELECT 1 FROM chain_steps e WHERE e.chain_id=c.id AND e.node_id=?1 AND e.block=?2))
            ORDER BY id DESC LIMIT 1000").map_err(sqlite_err)?;
        let rows = stmt
            .query_map(
                params![address.map(|a| a.0), address.map(|a| a.1), chain],
                read_summary,
            )
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    /// Read one consistent metadata snapshot; never load stdout or effective output.
    pub(crate) fn stats(
        &self,
        address: Option<(String, String)>,
        chain: bool,
        since: Option<(String, i64)>,
    ) -> io::Result<crate::run_stats::Report> {
        use crate::run_stats::Snapshot;
        use std::collections::BTreeMap;
        let (retained, runs) = {
            let conn = self.conn.lock().unwrap();
            let mut stmt=conn.prepare("SELECT id,live_id,node_id,block,path,source,options,status,exit_code,started_at,ended_at,truncated,output_disabled,incomplete,
            CAST(ROUND((julianday(ended_at)-julianday(started_at))*86400000) AS INTEGER) FROM chain_runs ORDER BY id").map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |r| Ok((read_summary(r)?, r.get::<_, Option<i64>>(14)?)))
                .map_err(sqlite_err)?;
            let rows = rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)?;
            let retained: Vec<_> = rows.iter().map(|(s, _)| s.clone()).collect();
            let mut stmt = conn
                .prepare("SELECT chain_id,node_id,block FROM chain_steps ORDER BY chain_id,rowid")
                .map_err(sqlite_err)?;
            let rows_steps = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                    ))
                })
                .map_err(sqlite_err)?;
            let mut steps = BTreeMap::<i64, Vec<(String, String)>>::new();
            for row in rows_steps {
                let (id, addr) = row.map_err(sqlite_err)?;
                steps.entry(id).or_default().push(addr);
            }
            let mut stmt=conn.prepare("SELECT seq,time,json_remove(event,'$.effective','$.output') FROM chain_events
            WHERE chain_id=?1 AND json_extract(event,'$.type') IN ('step-start','step-end','step-skipped','step-not-started','service-started','tty-start','killed') ORDER BY seq").map_err(sqlite_err)?;
            let mut runs = Vec::new();
            for (summary, duration) in rows {
                let plan = steps.remove(&summary.id).unwrap_or_default();
                if since.as_ref().is_some_and(|(_, cutoff)| {
                    meshfox_core::timestamp::unix_timestamp(&summary.started_at)
                        .is_none_or(|time| time < *cutoff)
                }) {
                    continue;
                }
                if let Some(addr) = &address {
                    let matches = if chain {
                        (summary.node_id.clone(), summary.block.clone()) == *addr
                    } else {
                        plan.contains(addr)
                    };
                    if !matches {
                        continue;
                    }
                }
                let events = stmt
                    .query_map([summary.id], |r| {
                        let text: String = r.get(2)?;
                        Ok(Event {
                            seq: r.get(0)?,
                            time: r.get(1)?,
                            event: serde_json::from_str(&text).map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    2,
                                    rusqlite::types::Type::Text,
                                    Box::new(e),
                                )
                            })?,
                        })
                    })
                    .map_err(sqlite_err)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_err)?;
                runs.push(Snapshot {
                    summary,
                    duration_ms: duration.and_then(|v| u64::try_from(v).ok()),
                    steps: plan,
                    events,
                });
            }
            (retained, runs)
        };
        Ok(crate::run_stats::summarize(
            &retained, runs, address, chain, since,
        ))
    }

    pub fn contains(&self, id: i64, address: (&str, &str)) -> io::Result<bool> {
        self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM chain_steps WHERE chain_id=?1 AND node_id=?2 AND block=?3)",params![id,address.0,address.1],|r|r.get(0)).map_err(sqlite_err)
    }

    pub fn page(
        &self,
        id: i64,
        address: Option<(&str, &str)>,
        since: i64,
        effective: bool,
    ) -> io::Result<Page> {
        let conn = self.conn.lock().unwrap();
        let selected = conn.query_row("SELECT id,live_id,node_id,block,path,source,options,status,exit_code,started_at,ended_at,truncated,output_disabled,incomplete FROM chain_runs WHERE id=?1",[id],read_summary).optional().map_err(sqlite_err)?;
        let Some(summary) = selected else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such chain run"));
        };
        let mut stmt = conn.prepare("SELECT seq,time,event FROM chain_events WHERE chain_id=?1 AND seq>=?2 AND (?3 IS NULL OR (node_id=?3 AND block=?4)) ORDER BY seq LIMIT 501").map_err(sqlite_err)?;
        let events = stmt
            .query_map(
                params![id, since, address.map(|a| a.0), address.map(|a| a.1)],
                |r| {
                    let raw: String = r.get(2)?;
                    Ok(Event {
                        seq: r.get(0)?,
                        time: r.get(1)?,
                        event: serde_json::from_str(&raw).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    })
                },
            )
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        let more = events.len() > 500;
        let mut events: Vec<_> = events.into_iter().take(500).collect();
        let next_seq = if more {
            events.last().unwrap().seq + 1
        } else {
            conn.query_row("SELECT next_seq FROM chain_runs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .map_err(sqlite_err)?
        };
        if !effective {
            for item in &mut events {
                item.event.as_object_mut().unwrap().remove("effective");
            }
        }
        Ok(Page {
            runs: Vec::new(),
            selected: Some(summary),
            events,
            next_seq,
            more,
        })
    }
}

fn read_summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    let path: String = r.get(4)?;
    let options: String = r.get(6)?;
    Ok(Summary {
        id: r.get(0)?,
        live_id: r.get(1)?,
        node_id: r.get(2)?,
        block: r.get(3)?,
        path: serde_json::from_str(&path).unwrap_or_default(),
        source: r.get(5)?,
        options: serde_json::from_str(&options).unwrap_or(Value::Null),
        status: r.get(7)?,
        exit_code: r.get(8)?,
        started_at: r.get(9)?,
        ended_at: r.get(10)?,
        truncated: r.get(11)?,
        output_disabled: r.get(12)?,
        incomplete: r.get(13)?,
    })
}

fn trim_text(value: &mut Value, remaining: &mut usize, truncated: &mut bool) {
    let Some(text) = value.as_str() else { return };
    let mut len = (*remaining).min(text.len());
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    *truncated |= len < text.len();
    *remaining -= len;
    if len < text.len() {
        *value = json!(&text[..len]);
    }
}

pub fn record(
    journal: RunJournal,
    id: i64,
    ledger: RunLedger,
    stream: impl futures_util::Stream<Item = Result<axum::body::Bytes, io::Error>> + Send + 'static,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, io::Error>> + Send + 'static {
    async_stream::stream! {
        use futures_util::StreamExt;
        struct Guard {journal:RunJournal,id:i64,pending:Vec<Value>,failed:bool}
        impl Guard {
            fn check(&mut self,result:io::Result<()>) {
                if let Err(error)=result {
                    if !self.failed {eprintln!("meshfox: could not save run #{} transcript: {error}",self.id);}
                    self.failed=true;
                }
                if self.failed {let _=self.journal.mark_incomplete(self.id);}
            }
            fn flush(&mut self) {
                let result=self.journal.append_outputs(self.id,&self.pending);
                self.check(result);
                self.pending.clear();
            }
        }
        impl Drop for Guard {fn drop(&mut self){self.flush();let _=self.journal.interrupt(self.id);}}
        let mut guard=Guard {journal,id,pending:Vec::new(),failed:false};
        let mut stream=Box::pin(stream);
        let mut tick=tokio::time::interval(std::time::Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let item=tokio::select! {
                item=stream.next()=>item,
                _=tick.tick()=>{guard.flush();continue;}
            };
            let Some(item)=item else {break};
            if let Ok(bytes)=&item {
                if let Ok(event)=serde_json::from_slice::<Value>(bytes) {
                    if event["type"]=="output" {
                        guard.pending.push(event);
                        if guard.pending.len()>=64 {guard.flush();}
                    } else {
                        guard.flush();
                        let result=guard.journal.append(id,event,&ledger);
                        guard.check(result);
                    }
                }
            } else {guard.failed=true;}
            yield item;
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Query {
    #[serde(default)]
    selector: String,
    id: Option<i64>,
    #[serde(default)]
    chain: bool,
    #[serde(default)]
    list: bool,
    #[serde(default)]
    effective: bool,
    #[serde(default)]
    since_seq: i64,
}

/// Shared historical-address resolution for logs and statistics.
pub(crate) fn resolve_selector(
    state: &crate::AppState,
    selector: &[String],
) -> Result<Option<(String, String)>, crate::ApiError> {
    use axum::http::StatusCode;
    let address = if let Some((block, path)) = selector.split_last() {
        let raw = state.raw.lock().unwrap().clone();
        let canvas = crate::resolved_canvas(&raw, &state.canvas_path).ok();
        let path_refs: Vec<_> = path.iter().map(String::as_str).collect();
        let resolved = canvas
            .as_ref()
            .and_then(|c| meshfox_core::resolve_run_chain(c, &path_refs, block, false).ok())
            .and_then(|c| c.last().cloned());
        let node = resolved
            .as_ref()
            .map(|a| a.node_id.clone())
            .or_else(|| {
                canvas
                    .as_ref()
                    .and_then(|c| c.resolve_path(&path_refs).ok())
                    .map(|n| n.id.clone())
            })
            .or_else(|| path.last().cloned())
            .or_else(|| {
                canvas
                    .as_ref()
                    .and_then(|c| c.root().ok())
                    .map(|n| n.id.clone())
            })
            .ok_or_else(|| {
                crate::ApiError(
                    StatusCode::BAD_REQUEST,
                    "cannot resolve block address".into(),
                )
            })?;
        Some((
            node,
            resolved.map_or_else(|| block.clone(), |a| a.block_name),
        ))
    } else {
        None
    };
    Ok(address)
}

pub(crate) async fn api(
    axum::extract::State(state): axum::extract::State<Arc<crate::AppState>>,
    axum::extract::Query(query): axum::extract::Query<Query>,
) -> Result<axum::Json<Page>, crate::ApiError> {
    use axum::http::StatusCode;
    let fail = |e: io::Error| {
        crate::ApiError(
            if e.kind() == io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            e.to_string(),
        )
    };
    let selector: Vec<String> = if query.selector.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&query.selector)
            .map_err(|e| crate::ApiError(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    let address = resolve_selector(&state, &selector)?;
    let borrowed = address.as_ref().map(|(n, b)| (n.as_str(), b.as_str()));
    let runs = if query.list || query.id.is_none() {
        state
            .run_journal
            .list(borrowed, query.chain)
            .map_err(fail)?
    } else {
        Vec::new()
    };
    if query.list {
        return Ok(axum::Json(Page {
            runs,
            selected: None,
            events: Vec::new(),
            next_seq: 0,
            more: false,
        }));
    }
    let id=query.id.or_else(||runs.first().map(|r|r.id)).ok_or_else(||crate::ApiError(StatusCode::NOT_FOUND,"no recorded run matches this selection (chain transcripts are recorded from this version onward)".into()))?;
    let filter = if query.chain { None } else { borrowed };
    let page = state
        .run_journal
        .page(id, filter, query.since_seq.max(0), query.effective)
        .map_err(fail)?;
    if let Some((node, block)) = filter {
        // An unknown address must not look like a valid empty transcript.
        let found = state
            .run_journal
            .contains(id, (node, block))
            .map_err(fail)?;
        let target = page
            .selected
            .as_ref()
            .is_some_and(|s| s.node_id == node && s.block == block);
        if !found && !target {
            return Err(crate::ApiError(
                StatusCode::NOT_FOUND,
                "block did not participate in this chain".into(),
            ));
        }
    }
    Ok(axum::Json(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        run_ledger::{FinishOutcome, RunKind},
        stream_exec::OutputStream,
    };
    fn setup(bytes: usize, keep: usize) -> (RunJournal, RunLedger) {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let journal = RunJournal::new(conn.clone(), bytes, keep).unwrap();
        let ledger = RunLedger::from_connection(conn)
            .unwrap()
            .with_max_runs_per_block(1);
        (journal, ledger)
    }
    fn start(journal: &RunJournal) -> i64 {
        journal
            .start("live", "root", "target", &[], "test", json!({}))
            .unwrap()
    }
    #[test]
    fn effective_output_is_an_immutable_snapshot_of_the_exact_source() {
        let (journal, ledger) = setup(1000, 10);
        let source = ledger
            .start("root", "dep", RunKind::Plain, "test", 0)
            .unwrap();
        ledger
            .save_output(
                source,
                &[
                    (OutputStream::Stdout, "old".into()),
                    (OutputStream::Stderr, "warning".into()),
                ],
            )
            .unwrap();
        ledger.finish(source, FinishOutcome::Exited(0)).unwrap();
        let chain = start(&journal);
        journal.append(chain,json!({"type":"step-skipped","nodeId":"root","block":"dep","sourceRunId":source,"output":"must not be included in the default view"}),&ledger).unwrap();
        journal
            .append(chain, json!({"type":"done","exitCode":0}), &ledger)
            .unwrap();
        let new = ledger
            .start("root", "dep", RunKind::Plain, "test", 0)
            .unwrap();
        ledger
            .save_output(new, &[(OutputStream::Stdout, "new".into())])
            .unwrap();
        ledger.finish(new, FinishOutcome::Exited(0)).unwrap();
        assert!(ledger.load_output(source).unwrap().is_none());
        let plain = journal
            .page(chain, Some(("root", "dep")), 0, false)
            .unwrap();
        assert!(plain.events[0].event.get("output").is_none());
        assert!(plain.events[0].event.get("effective").is_none());
        let effective = journal.page(chain, None, 0, true).unwrap();
        assert_eq!(effective.events[0].event["effective"]["runId"], source);
        assert_eq!(
            effective.events[0].event["effective"]["lines"][0]["text"],
            "old"
        );
        assert_eq!(
            effective.events[0].event["effective"]["lines"][1]["stream"],
            "stderr"
        );
    }
    #[test]
    fn restart_retains_prefix_and_marks_incomplete_chain_interrupted() {
        let (journal, ledger) = setup(1000, 10);
        let chain = start(&journal);
        journal.append(chain,json!({"type":"output","nodeId":"root","block":"target","stream":"stdout","text":"before crash"}),&ledger).unwrap();
        let reopened = RunJournal::new(journal.conn.clone(), 1000, 10).unwrap();
        let page = reopened.page(chain, None, 0, false).unwrap();
        assert_eq!(page.selected.unwrap().status, "interrupted");
        assert_eq!(page.events[0].event["text"], "before crash");
    }
    #[test]
    fn byte_limits_preserve_utf8_and_terminal_metadata_and_history_rotates() {
        let (journal, ledger) = setup(3, 1);
        let chain = start(&journal);
        journal
            .append(chain, json!({"type":"output","text":"абв"}), &ledger)
            .unwrap();
        journal
            .append(chain, json!({"type":"done","exitCode":7}), &ledger)
            .unwrap();
        let page = journal.page(chain, None, 0, false).unwrap();
        assert_eq!(page.events[0].event["text"], "а");
        assert!(page.selected.as_ref().unwrap().truncated);
        assert_eq!(page.selected.unwrap().status, "failed");
        let next = start(&journal);
        journal
            .append(next, json!({"type":"done","exitCode":0}), &ledger)
            .unwrap();
        assert!(journal.page(chain, None, 0, false).is_err());
        assert_eq!(journal.list(None, true).unwrap().len(), 1);
    }
    #[test]
    fn disabled_output_keeps_step_and_completion_records() {
        let (journal, ledger) = setup(0, 10);
        let chain = start(&journal);
        journal
            .append(
                chain,
                json!({"type":"step-start","nodeId":"root","block":"target"}),
                &ledger,
            )
            .unwrap();
        journal
            .append(
                chain,
                json!({"type":"output","text":"secret output disabled"}),
                &ledger,
            )
            .unwrap();
        journal
            .append(chain, json!({"type":"done","exitCode":0}), &ledger)
            .unwrap();
        let page = journal.page(chain, None, 0, false).unwrap();
        assert!(page.selected.unwrap().output_disabled);
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.next_seq, 3);
    }
    #[test]
    fn missing_source_is_explicit_and_does_not_substitute_a_newer_run() {
        let (journal, ledger) = setup(1000, 10);
        let chain = start(&journal);
        journal
            .append(
                chain,
                json!({"type":"step-skipped","nodeId":"root","block":"dep","sourceRunId":42}),
                &ledger,
            )
            .unwrap();
        assert_eq!(
            journal.page(chain, None, 0, true).unwrap().events[0].event["effective"]["available"],
            false
        );
    }
    #[tokio::test]
    async fn recorder_batches_output_and_marks_storage_failure_without_changing_execution_stream() {
        use futures_util::StreamExt;
        let (journal, ledger) = setup(1000, 10);
        let chain = start(&journal);
        journal.conn.lock().unwrap().execute_batch("CREATE TRIGGER reject_output BEFORE INSERT ON chain_events WHEN json_extract(NEW.event,'$.type')='output' BEGIN SELECT RAISE(ABORT,'simulated write failure'); END;").unwrap();
        let values = vec![
            json!({"type":"step-start","nodeId":"root","block":"target"}),
            json!({"type":"output","nodeId":"root","block":"target","text":"still delivered to the live client"}),
            json!({"type":"done","exitCode":0}),
        ];
        let source = futures_util::stream::iter(
            values
                .iter()
                .map(|v| Ok(axum::body::Bytes::from(v.to_string())))
                .collect::<Vec<_>>(),
        );
        let delivered: Vec<_> = record(journal.clone(), chain, ledger, source)
            .collect()
            .await;
        assert_eq!(delivered.len(), 3);
        assert!(String::from_utf8(delivered[1].as_ref().unwrap().to_vec())
            .unwrap()
            .contains("still delivered"));
        let page = journal.page(chain, None, 0, false).unwrap();
        let summary = page.selected.unwrap();
        assert!(summary.incomplete);
        assert_eq!(summary.status, "succeeded");
        assert_eq!(page.events.len(), 2);
    }

    #[tokio::test]
    async fn output_batch_is_available_before_a_quiet_run_finishes() {
        use futures_util::StreamExt;
        let (journal, ledger) = setup(1000, 10);
        let chain = start(&journal);
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<axum::body::Bytes, io::Error>>();
        let source = async_stream::stream! {while let Some(event)=rx.recv().await {yield event;}};
        let consumer =
            tokio::spawn(record(journal.clone(), chain, ledger, source).collect::<Vec<_>>());
        tx.send(Ok(axum::body::Bytes::from(
            json!({"type":"output","nodeId":"root","block":"target","text":"live prefix"})
                .to_string(),
        )))
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if !journal
                    .page(chain, None, 0, false)
                    .unwrap()
                    .events
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            journal
                .page(chain, None, 0, false)
                .unwrap()
                .selected
                .unwrap()
                .status,
            "running"
        );
        drop(tx);
        consumer.await.unwrap();
        assert_eq!(
            journal
                .page(chain, None, 0, false)
                .unwrap()
                .selected
                .unwrap()
                .status,
            "interrupted"
        );
    }
}

#[cfg(test)]
mod stats_snapshot_tests {
    use super::*;
    fn setup(keep: usize) -> (RunJournal, RunLedger) {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let journal = RunJournal::new(conn.clone(), 1000, keep).unwrap();
        (journal, RunLedger::from_connection(conn).unwrap())
    }
    fn chain(journal: &RunJournal, ledger: &RunLedger, target: &str, day: u32) -> i64 {
        let id = journal
            .start(
                "live",
                "root",
                target,
                &[],
                "test",
                json!({"steps":[{"nodeId":"root","block":"dep"},{"nodeId":"root","block":target}]}),
            )
            .unwrap();
        journal
            .append(
                id,
                json!({"type":"step-start","nodeId":"root","block":"dep"}),
                ledger,
            )
            .unwrap();
        journal.append(id,json!({"type":"step-end","nodeId":"root","block":"dep","exitCode":0,"durationMs":100}),ledger).unwrap();
        journal
            .append(id, json!({"type":"done","exitCode":0}), ledger)
            .unwrap();
        journal
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE chain_runs SET started_at=?2,ended_at=?3 WHERE id=?1",
                params![
                    id,
                    format!("2026-10-{day:02}T10:00:00.000Z"),
                    format!("2026-10-{day:02}T10:00:01.250Z")
                ],
            )
            .unwrap();
        id
    }
    #[test]
    fn retained_coverage_cutoff_and_target_vs_block_filters_use_one_snapshot() {
        let (journal, ledger) = setup(2);
        chain(&journal, &ledger, "old", 18);
        chain(&journal, &ledger, "target", 19);
        chain(&journal, &ledger, "other", 20);
        let all = journal.stats(None, false, None).unwrap();
        assert_eq!(all.coverage.retained_chains, 2);
        assert_eq!(
            all.coverage.retained_from.as_deref(),
            Some("2026-10-19T10:00:00.000Z")
        );
        assert_eq!(all.chain_duration.median_ms, Some(1250.0));
        let cutoff = crate::run_stats::parse_since("2026-10-20T14:00:00+04:00").unwrap();
        let last = journal
            .stats(
                None,
                false,
                Some(("2026-10-20T14:00:00+04:00".into(), cutoff)),
            )
            .unwrap();
        assert_eq!(last.coverage.retained_chains, 2);
        assert_eq!(last.coverage.matching_chains, 1);
        assert_eq!(
            last.coverage.from.as_deref(),
            Some("2026-10-20T10:00:00.000Z")
        );
        let dep = journal
            .stats(Some(("root".into(), "dep".into())), false, None)
            .unwrap();
        assert_eq!(dep.coverage.matching_chains, 2);
        assert_eq!(dep.blocks.len(), 1);
        assert_eq!(dep.blocks[0].outcomes.succeeded, 2);
        let target = journal
            .stats(Some(("root".into(), "target".into())), true, None)
            .unwrap();
        assert_eq!(target.coverage.matching_chains, 1);
        assert_eq!(
            target
                .blocks
                .iter()
                .find(|b| b.block == "target")
                .unwrap()
                .not_started,
            1
        );
        let dep_target = journal
            .stats(Some(("root".into(), "dep".into())), true, None)
            .unwrap();
        assert_eq!(dep_target.coverage.matching_chains, 0);
    }
    #[test]
    fn statistics_are_not_capped_by_the_logs_list_limit() {
        let (journal, ledger) = setup(1010);
        for _ in 0..1003 {
            let id = journal
                .start("live", "root", "target", &[], "test", json!({}))
                .unwrap();
            journal
                .append(id, json!({"type":"done","exitCode":0}), &ledger)
                .unwrap();
        }
        assert_eq!(journal.list(None, true).unwrap().len(), 1000);
        let stats = journal.stats(None, false, None).unwrap();
        assert_eq!(stats.coverage.retained_chains, 1003);
        assert_eq!(stats.chains.succeeded, 1003);
    }
}
