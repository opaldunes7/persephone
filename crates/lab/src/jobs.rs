//! Slow work runs as a job: a call waits a while for it, and otherwise hands back an id to check on,
//! so an agent's connection is never held hostage by a long batch.

use std::{
    collections::HashMap,
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{Lab, new_id};

/// How long a call waits for its job unless told otherwise.
pub const DEFAULT_WAIT_SECONDS: f64 = 30.0;

/// Jobs under way or finished since Lab started. Earlier ones are read from their files.
pub type Jobs = HashMap<String, Arc<Job>>;

/// A piece of slow work.
pub struct Job {
    pub tool: String,
    /// How far it's got, when that can be told.
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    /// Set to ask it to stop.
    pub stop: Arc<AtomicBool>,
    result: Mutex<Option<Result<Value, String>>>,
}

impl Job {
    fn finished(&self) -> Option<Result<Value, String>> {
        self.result.lock().ok()?.clone()
    }
}

/// What a job's status looks like, finished or not.
fn status(id: &str, job: &Job) -> Result<Value, String> {
    match job.finished() {
        Some(Ok(result)) => Ok(json!({
            "job_id": id,
            "tool": job.tool,
            "status": "done",
            "result": result,
        })),
        Some(Err(error)) => Err(error),
        None if job.stop.load(Ordering::Relaxed) => Ok(json!({
            "job_id": id,
            "tool": job.tool,
            "status": "cancelling",
            "done": job.done.load(Ordering::Relaxed),
            "total": job.total.load(Ordering::Relaxed),
        })),
        None => Ok(json!({
            "job_id": id,
            "tool": job.tool,
            "status": "running",
            "done": job.done.load(Ordering::Relaxed),
            "total": job.total.load(Ordering::Relaxed),
        })),
    }
}

impl Lab {
    /// Runs `work` on its own thread, waiting up to the call's `wait_seconds` for it. It answers with
    /// the result if that's enough, or a job to check on with `get_job`.
    pub(crate) fn run_job(
        self: &Arc<Self>,
        tool: &str,
        args: &Value,
        work: impl FnOnce(&Arc<Self>, &Arc<Job>) -> Result<Value, String> + Send + 'static,
    ) -> Result<Value, String> {
        let id = new_id("job");
        let job = Arc::new(Job {
            tool: tool.to_owned(),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            stop: Arc::new(AtomicBool::new(false)),
            result: Mutex::new(None),
        });
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(id.clone(), Arc::clone(&job));
        }
        let (lab, worker, file) = (
            Arc::clone(self),
            Arc::clone(&job),
            self.paths.lab().join("jobs").join(format!("{id}.json")),
        );
        let tool_name = tool.to_owned();
        let job_id = id.clone();
        thread::spawn(move || {
            let result = thread::scope(|scope| {
                scope
                    .spawn(|| work(&lab, &worker))
                    .join()
                    .unwrap_or_else(|_| Err("The work stopped unexpectedly.".to_owned()))
            });
            // Kept so the answer outlasts this process.
            let saved = match &result {
                Ok(value) => json!({"tool": tool_name, "status": "done", "result": value}),
                Err(error) => json!({"tool": tool_name, "status": "failed", "error": error}),
            };
            let _ = fs::write(file, saved.to_string());
            if let Ok(mut slot) = worker.result.lock() {
                *slot = Some(result);
            }
            // Its answer's on disk now, so a long-lived server needn't keep it too.
            if let Ok(mut jobs) = lab.jobs.lock() {
                jobs.remove(&job_id);
            }
        });

        let wait = args
            .get("wait_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(DEFAULT_WAIT_SECONDS)
            .max(0.0);
        let deadline = Instant::now() + Duration::from_secs_f64(wait.min(86_400.0));
        loop {
            if let Some(result) = job.finished() {
                return result.map(|mut value| {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("job_id".into(), json!(id));
                    }
                    value
                });
            }
            if Instant::now() >= deadline {
                return status(&id, &job);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub(crate) fn get_job(&self, args: &Value) -> Result<Value, String> {
        let id = crate::tools::text(args, "job_id")?;
        let job = self.jobs.lock().ok().and_then(|jobs| jobs.get(id).cloned());
        if let Some(job) = job {
            return status(id, &job);
        }
        check_id(id)?;
        let file = self.paths.lab().join("jobs").join(format!("{id}.json"));
        let text = fs::read_to_string(file).map_err(|_| format!("there's no job `{id}`"))?;
        let mut saved: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if saved["status"] == "failed" {
            return Err(saved["error"].as_str().unwrap_or("It failed.").to_owned());
        }
        if let Some(object) = saved.as_object_mut() {
            object.insert("job_id".into(), json!(id));
        }
        Ok(saved)
    }

    pub(crate) fn cancel_job(&self, args: &Value) -> Result<Value, String> {
        let id = crate::tools::text(args, "job_id")?;
        let job = self
            .jobs
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(id).cloned())
            .ok_or_else(|| format!("there's no running job `{id}`"))?;
        job.stop.store(true, Ordering::Relaxed);
        status(id, &job)
    }
}

/// An id from an argument becomes a file name, so it mustn't lead anywhere else.
pub(crate) fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("`{id}` isn't an id"));
    }
    Ok(())
}
