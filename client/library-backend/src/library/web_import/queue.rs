//! Import retry, replay and source retention, independent of worker connections.
use client_platform_web::transport::{field, object};
use js_sys::{Array, Object, Reflect};
use std::collections::BTreeMap;
use wasm_bindgen::JsValue;

struct Entry<R> {
    job: JsValue,
    reply: Option<R>,
    status: JsValue,
}
pub struct ImportJobs<R> {
    jobs: BTreeMap<String, Entry<R>>,
}
impl<R> Default for ImportJobs<R> {
    fn default() -> Self {
        Self { jobs: BTreeMap::new() }
    }
}
impl<R> ImportJobs<R> {
    pub fn submit(&mut self, job: JsValue, reply: Option<R>) -> Option<JsValue> {
        let id = field(&job, "id").as_string()?;
        if self.jobs.contains_key(&id) {
            return None;
        }
        let status = object(&[("transport", "import_progress".into()), ("id", id.clone().into()), ("name", field(&job, "name")), ("phase", "queued".into()), ("totalFiles", Array::from(&field(&job, "files")).length().into())]);
        self.jobs.insert(id, Entry { job, reply, status: status.clone() });
        Some(status)
    }
    pub fn statuses(&self) -> Vec<JsValue> {
        self.jobs.values().map(|entry| entry.status.clone()).collect()
    }
    pub fn pending(&self) -> Vec<JsValue> {
        self.jobs.values().filter(|entry| !matches!(field(&entry.status, "phase").as_string().as_deref(), Some("complete" | "failed"))).map(|entry| entry.job.clone()).collect()
    }
    pub fn retry(&mut self, id: &str) -> Option<(JsValue, JsValue)> {
        let entry = self.jobs.get_mut(id)?;
        if field(&entry.status, "phase").as_string().as_deref() != Some("failed") {
            return None;
        }
        let status = Object::assign(&Object::new(), &Object::from(entry.status.clone()));
        Reflect::set(&status, &"phase".into(), &"queued".into()).unwrap();
        Reflect::delete_property(&status, &"error".into()).unwrap();
        Reflect::delete_property(&status, &"storageFull".into()).unwrap();
        entry.status = status.into();
        Some((entry.status.clone(), entry.job.clone()))
    }
    pub fn progress(&mut self, data: &JsValue) -> Option<JsValue> {
        let id = field(data, "id").as_string()?;
        let entry = self.jobs.get_mut(&id)?;
        entry.status = data.clone();
        Some(entry.status.clone())
    }
    pub fn finish(&mut self, data: &JsValue) -> Option<(JsValue, Option<R>)> {
        let id = field(data, "id").as_string()?;
        let entry = self.jobs.get_mut(&id)?;
        let error = field(data, "error");
        let failed = error.is_truthy();
        let status = Object::assign(&Object::new(), &Object::from(entry.status.clone()));
        for (key, value) in [("transport", "import_progress".into()), ("phase", if failed { "failed" } else { "complete" }.into()), ("error", error), ("storageFull", field(data, "storageFull")), ("failures", field(data, "failures"))] {
            Reflect::set(&status, &key.into(), &value).unwrap();
        }
        entry.status = status.into();
        if !failed {
            Reflect::set(&entry.job, &"files".into(), &Array::new()).unwrap();
        }
        Some((entry.status.clone(), entry.reply.take()))
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn status(&self, id: &str) -> JsValue {
        self.jobs[id].status.clone()
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn has_reply(&self, id: &str) -> bool {
        self.jobs[id].reply.is_some()
    }
}
